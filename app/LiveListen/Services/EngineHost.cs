using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text.Json.Nodes;
using LiveListen.Interop;
using Microsoft.UI.Dispatching;

namespace LiveListen.Services;

/// <summary>Owns the Rust engine and forwards its JSON events to the UI thread.</summary>
internal sealed class EngineHost : IDisposable
{
    private static EngineHost? current;
    private readonly DispatcherQueue dispatcher;
    /// <summary>Calls hold it shared; <see cref="Dispose"/> takes it exclusively, so the
    /// native handle is never freed while a background call still uses it.</summary>
    private readonly ReaderWriterLockSlim gate = new();
    private readonly PendingCalls calls = new();
    private nint handle;
    private Task? disposing;

    /// <summary>Bounds lock acquisition only. Accepted calls are drained separately;
    /// each native request has its own 20-second reply timeout.</summary>
    private static readonly TimeSpan DisposeWait = TimeSpan.FromSeconds(25);

    public event Action<string, JsonObject>? EventReceived;

    /// <summary>Settings folder from <c>LIVE_LISTEN_CONFIG_DIR</c> for test runs; null uses the
    /// user's own <c>%APPDATA%\LiveListen</c>. A test run is a separate instance and leaves the
    /// Windows startup entry alone.</summary>
    public static readonly string? TestConfigDir = ReadTestConfigDir();

    private static string? ReadTestConfigDir()
    {
        var value = Environment.GetEnvironmentVariable("LIVE_LISTEN_CONFIG_DIR");
        return string.IsNullOrWhiteSpace(value) ? null : Path.GetFullPath(value);
    }

    private EngineHost(DispatcherQueue dispatcher)
    {
        this.dispatcher = dispatcher;
    }

    public static string Version
    {
        get
        {
            try
            {
                return Marshal.PtrToStringUTF8(Native.ll_version()) ?? "?";
            }
            catch (Exception e) when (IsLoadError(e))
            {
                return "?";
            }
        }
    }

    /// <summary>Starts the engine; returns null with a reason when the core cannot load.</summary>
    public static unsafe EngineHost? Start(DispatcherQueue dispatcher, out string? error)
    {
        var host = new EngineHost(dispatcher);
        current = host;
        try
        {
            host.handle = Native.ll_start(TestConfigDir, &OnEvent, 0);
        }
        catch (Exception e) when (IsLoadError(e))
        {
            error = "core_missing";
            current = null;
            return null;
        }
        if (host.handle == 0)
        {
            error = "core_failed";
            current = null;
            return null;
        }
        error = null;
        return host;
    }

    private static bool IsLoadError(Exception e) =>
        e is DllNotFoundException or EntryPointNotFoundException or BadImageFormatException;

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    private static void OnEvent(nint json, nint user)
    {
        var host = current;
        var text = Marshal.PtrToStringUTF8(json);
        if (host is null || text is null)
        {
            return;
        }
        host.dispatcher.TryEnqueue(() => host.Raise(text));
    }

    private void Raise(string text)
    {
        if (!calls.Accepting || current != this)
        {
            return;
        }
        if (JsonNode.Parse(text) is JsonObject value && value["event"]?.GetValue<string>() is { } name)
        {
            EventReceived?.Invoke(name, value);
        }
    }

    /// <summary>Runs one command synchronously. Safe from any thread; after
    /// <see cref="Dispose"/> started it answers "stopped".</summary>
    public JsonObject Call(string command, JsonObject? args = null)
        => calls.Run(() => Task.FromResult(CallCore(command, args)))?.GetAwaiter().GetResult() ?? Stopped();

    private JsonObject CallCore(string command, JsonObject? args)
    {
        if (!TryEnterRead())
        {
            return Stopped();
        }
        try
        {
            if (handle == 0)
            {
                return Stopped();
            }
            var request = args ?? new JsonObject();
            request["cmd"] = command;
            var reply = Native.ll_call(handle, request.ToJsonString());
            try
            {
                var text = Marshal.PtrToStringUTF8(reply) ?? "{}";
                return JsonNode.Parse(text) as JsonObject ?? new JsonObject();
            }
            finally
            {
                Native.ll_free(reply);
            }
        }
        finally
        {
            gate.ExitReadLock();
        }
    }

    /// <summary>Waits for the gate unless the engine is going away.</summary>
    private bool TryEnterRead()
    {
        try
        {
            return handle != 0 && gate.TryEnterReadLock(DisposeWait);
        }
        catch (ObjectDisposedException)
        {
            return false;
        }
    }

    private static JsonObject Stopped() => new() { ["ok"] = false, ["error"] = "stopped" };

    public Task<JsonObject> CallAsync(string command, JsonObject? args = null) =>
        calls.Run(() => Task.Run(() => CallCore(command, args))) ?? Task.FromResult(Stopped());

    /// <summary>Preserves the running core when final persistence fails, so saving can be retried.</summary>
    public async Task<JsonObject> TryCloseAsync()
    {
        await calls.StopAsync().ConfigureAwait(false);
        JsonObject reply;
        try
        {
            // Admission is paused: only this command may reach the core after the drain.
            reply = await Task.Run(() => CallCore("shutdown", null)).ConfigureAwait(false);
        }
        catch
        {
            calls.Resume();
            throw;
        }
        if (!reply.Ok())
        {
            calls.Resume();
            return reply;
        }
        await DisposeAsync().ConfigureAwait(false);
        return reply;
    }

    /// <summary>Drains accepted calls before destroying the core, without blocking the UI.</summary>
    public Task DisposeAsync() => disposing ??= StopAsync();

    private async Task StopAsync()
    {
        await calls.StopAsync().ConfigureAwait(false);
        await Task.Run(Destroy).ConfigureAwait(false);
    }

    /// <summary>Final disposal fallback: drains accepted calls, then destroys the core.
    /// Interactive exit uses TryCloseAsync first so persistence failure can cancel exit.
    /// If the native lock cannot be acquired, the handle is left allocated rather than
    /// freed underneath a caller.</summary>
    public void Dispose()
        => DisposeAsync().GetAwaiter().GetResult();

    private void Destroy()
    {
        if (!gate.TryEnterWriteLock(DisposeWait))
        {
            handle = 0;
            current = null;
            return;
        }
        nint h;
        try
        {
            h = handle;
            handle = 0;
        }
        finally
        {
            gate.ExitWriteLock();
        }
        if (h != 0)
        {
            Native.ll_destroy(h);
        }
        current = null;
    }
}

internal static class Json
{
    public static bool Ok(this JsonObject reply) => reply["ok"]?.GetValue<bool>() == true;

    public static string Str(this JsonNode? node, string fallback = "") =>
        node is JsonValue v && v.TryGetValue(out string? s) ? s : fallback;

    public static bool Bool(this JsonNode? node, bool fallback = false) =>
        node is JsonValue v && v.TryGetValue(out bool b) ? b : fallback;

    public static double Num(this JsonNode? node, double fallback = 0)
    {
        if (node is not JsonValue v)
        {
            return fallback;
        }
        if (v.TryGetValue(out double d))
        {
            return d;
        }
        if (v.TryGetValue(out long l))
        {
            return l;
        }
        if (v.TryGetValue(out int i))
        {
            return i;
        }
        return fallback;
    }
}
