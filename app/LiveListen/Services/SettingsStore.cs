using System.Text.Json.Nodes;
using Microsoft.UI.Dispatching;

namespace LiveListen.Services;

/// <summary>
/// The settings tree as the engine returns it. Changes are applied locally at once (for live
/// preview) and saved through the engine after a short pause.
/// </summary>
internal sealed class SettingsStore
{
    private readonly EngineHost engine;
    private readonly DispatcherQueueTimer saveTimer;
    private readonly SettingsSaveQueue saves;

    public JsonObject Root { get; private set; } = new();

    /// <summary>Raised after any change; the argument is the changed path or "*" for all.</summary>
    public event Action<string>? Changed;

    /// <summary>Raised when the engine corrected values on save (field, code).</summary>
    public event Action<IReadOnlyList<(string Field, string Code)>>? Issues;

    /// <summary>Raised with the error code when the engine did not save the settings. The
    /// changes stay here and are sent again with the next change or on exit.</summary>
    public event Action<string>? SaveFailed;

    public SettingsStore(EngineHost engine, DispatcherQueue dispatcher)
    {
        this.engine = engine;
        saves = new SettingsSaveQueue(
            () => (JsonObject)Root.DeepClone(),
            value => engine.CallAsync("settings.set", new JsonObject { ["settings"] = value }),
            ApplySaved,
            code => SaveFailed?.Invoke(code));
        saveTimer = dispatcher.CreateTimer();
        saveTimer.Interval = TimeSpan.FromMilliseconds(350);
        saveTimer.IsRepeating = false;
        saveTimer.Tick += async (_, _) => await saves.SaveAsync();
    }

    public void Load()
    {
        var reply = engine.Call("settings.get");
        if (reply["settings"] is JsonObject settings)
        {
            Root = settings;
            Changed?.Invoke("*");
        }
    }

    public JsonNode? Get(string path)
    {
        JsonNode? node = Root;
        foreach (var part in path.Split('.'))
        {
            node = node is JsonObject obj ? obj[part] : null;
        }
        return node;
    }

    public string Str(string path, string fallback = "") => Get(path).Str(fallback);

    public bool Bool(string path) => Get(path).Bool();

    public double Num(string path, double fallback = 0) => Get(path).Num(fallback);

    public void Set(string path, JsonNode? value)
    {
        var parts = path.Split('.');
        JsonObject obj = Root;
        foreach (var part in parts[..^1])
        {
            if (obj[part] is not JsonObject child)
            {
                child = new JsonObject();
                obj[part] = child;
            }
            obj = child;
        }
        if (JsonNode.DeepEquals(obj[parts[^1]], value))
        {
            return; // Controls re-raise their current value when they load.
        }
        obj[parts[^1]] = value;
        saves.Edited();
        Changed?.Invoke(path);
        saveTimer.Stop();
        saveTimer.Start();
    }

    /// <summary>Replaces a whole section, e.g. when restoring defaults.</summary>
    public void SetSection(string path, JsonNode value) => Set(path, value.DeepClone());

    public async Task<JsonObject> DefaultsAsync()
    {
        var reply = await engine.CallAsync("settings.defaults");
        return reply["settings"] as JsonObject ?? new JsonObject();
    }

    public Task<bool> SaveNowAsync()
    {
        saveTimer.Stop();
        return saves.SaveAsync();
    }

    private void ApplySaved(JsonObject reply)
    {
        if (reply["settings"] is JsonObject saved)
        {
            // IPC may reorder properties or serialize 24 as 24.0. Those equivalent
            // values must not rebuild controls (and replay their entrance animations).
            var changed = !JsonNode.DeepEquals(saved, Root);
            Root = saved;
            if (changed)
            {
                Changed?.Invoke("*");
            }
        }
        if (reply["issues"] is JsonArray issues && issues.Count > 0)
        {
            var list = issues.OfType<JsonObject>().Select(i => (i["field"].Str(), i["code"].Str())).ToList();
            Issues?.Invoke(list);
        }
    }
}
