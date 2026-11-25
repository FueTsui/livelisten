using System.Buffers.Binary;
using System.Globalization;
using System.IO.Pipes;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace LiveListen.Services;

/// <summary>A local, same-user entry point for adding a card through the running engine.</summary>
internal static class CardCommandServer
{
    // Keep the wire limits in sync with the Rust client in win/cards_pipe.rs.
    private const int MaximumRequestBytes = 32 * 1024;
    private static readonly TimeSpan FrameTimeout = TimeSpan.FromSeconds(3);
    private static readonly DateTimeOffset FirstDateUtc = new(2000, 1, 1, 0, 0, 0, TimeSpan.Zero);
    private static readonly DateTimeOffset LastDateUtc = new DateTimeOffset(2100, 1, 1, 0, 0, 0, TimeSpan.Zero).AddMilliseconds(-1);

    public static async Task RunAsync(string name, Func<JsonObject, Task<JsonObject>> add, CancellationToken stop)
    {
        using var identity = WindowsIdentity.GetCurrent();
        var user = identity.User ?? throw new InvalidOperationException("Missing Windows user SID.");
        var security = new PipeSecurity();
        security.SetOwner(user);
        security.SetAccessRuleProtection(isProtected: true, preserveInheritance: false);
        var network = new SecurityIdentifier(WellKnownSidType.NetworkSid, null);
        security.AddAccessRule(new PipeAccessRule(network, PipeAccessRights.FullControl, AccessControlType.Deny));
        security.AddAccessRule(new PipeAccessRule(user, PipeAccessRights.FullControl, AccessControlType.Allow));

        while (!stop.IsCancellationRequested)
        {
            using var pipe = NamedPipeServerStreamAcl.Create(name, PipeDirection.InOut, 1,
                PipeTransmissionMode.Byte, PipeOptions.Asynchronous, 0, 0, security);
            try
            {
                await pipe.WaitForConnectionAsync(stop);
                var request = await ReadRequestAsync(pipe, stop);
                var reply = request is null ? Error("bad_request") : await add(request);
                await WriteReplyAsync(pipe, reply, stop);
            }
            catch (IOException)
            {
                // A disconnected client must not stop the listener or replay its request.
            }
            catch (OperationCanceledException)
            {
                // An idle client timed out, or the entire listener is stopping.
            }
        }
    }

    public static JsonObject Error(string code) => new() { ["ok"] = false, ["error"] = code };

    private static async Task<JsonObject?> ReadRequestAsync(Stream pipe, CancellationToken stop)
    {
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(stop);
        timeout.CancelAfter(FrameTimeout);
        var length = new byte[sizeof(int)];
        await pipe.ReadExactlyAsync(length, timeout.Token);
        var size = BinaryPrimitives.ReadInt32LittleEndian(length);
        if (size <= 0 || size > MaximumRequestBytes) return null;

        var payload = new byte[size];
        await pipe.ReadExactlyAsync(payload, timeout.Token);
        try
        {
            using var json = JsonDocument.Parse(payload);
            var root = json.RootElement;
            if (root.ValueKind != JsonValueKind.Object || StringField(root, "command") != "cards.add") return null;
            var kind = StringField(root, "kind");
            var text = StringField(root, "text");
            if (kind is not ("note" or "todo") || text is null) return null;

            var request = new JsonObject { ["kind"] = kind, ["text"] = text, ["strictText"] = true };
            if (root.TryGetProperty("date", out var field) && field.ValueKind != JsonValueKind.Null)
            {
                if (field.ValueKind != JsonValueKind.String ||
                    !DateOnly.TryParseExact(field.GetString(), "yyyy-MM-dd", CultureInfo.InvariantCulture, DateTimeStyles.None, out var date) ||
                    date.Year < FirstDateUtc.Year || date.Year > LastDateUtc.Year)
                {
                    return null;
                }
                // Use a local daytime hour to avoid transitions near midnight. At
                // the year limits, keep the date inside the core's UTC bounds.
                var time = new DateTimeOffset(date.ToDateTime(new TimeOnly(12, 0), DateTimeKind.Local));
                if (time < FirstDateUtc) time = FirstDateUtc;
                if (time > LastDateUtc) time = LastDateUtc;
                request["createdMs"] = time.ToUnixTimeMilliseconds();
            }
            return request;
        }
        catch (Exception error) when (error is JsonException or InvalidOperationException)
        {
            return null;
        }
    }

    private static string? StringField(JsonElement obj, string name) =>
        obj.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;

    private static async Task WriteReplyAsync(Stream pipe, JsonObject reply, CancellationToken stop)
    {
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(stop);
        timeout.CancelAfter(FrameTimeout);
        var payload = Encoding.UTF8.GetBytes(reply.ToJsonString());
        var length = new byte[sizeof(int)];
        BinaryPrimitives.WriteInt32LittleEndian(length, payload.Length);
        await pipe.WriteAsync(length, timeout.Token);
        await pipe.WriteAsync(payload, timeout.Token);
    }
}
