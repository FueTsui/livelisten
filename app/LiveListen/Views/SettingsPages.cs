using System.Text.Json.Nodes;
using LiveListen.Controls;
using LiveListen.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace LiveListen.Views;

internal sealed partial class SettingsWindow
{
    private string? recordingField;
    private Action? afterOwnChange;

    private static string T(string key) => Loc.T(key);

    // ---- 听写 -----------------------------------------------------------------------------

    private void BuildDictation()
    {
        var mic = Combo("dictation.microphone", [("", T("dict.mic.default"))], 280);
        Card(T("dict.mic.title"), null, mic, "");
        _ = FillMicrophones(mic);
        Block(T("dict.preconnect.title"), T("dict.preconnect.desc"), Radios("dictation.preconnectLevel",
            ("off", T("preconnect.off.desc")),
            ("balanced", T("preconnect.balanced.desc")),
            ("aggressive", T("preconnect.aggressive.desc"))));
        dictationTextToggle = Toggle("dictation.showText");
        Card(T("dict.showText.title"), T("dict.showText.desc"), dictationTextToggle);
        Block(T("dict.writeMode.title"), T("dict.writeMode.desc"), Radios("dictation.writeMode",
            ("stable", T("writeMode.stable.desc")),
            ("live", T("writeMode.live.desc"))));
        Block(T("dict.endAction.title"), T("dict.endAction.desc"), Radios("dictation.endAction",
            ("endOnly", T("endAction.endOnly.desc")),
            ("endAndReplay", T("endAction.endAndReplay.desc"))));
        Card(T("dict.rules.title"), T("dict.rules.desc"), null, "");
    }

    private Slider DictationFontSlider()
    {
        var slider = new Slider
        {
            Minimum = 12, Maximum = 64, StepFrequency = 1,
            Value = Math.Clamp(s.Num("dictation.fontSize", 16), 12, 64), Width = 220,
        };
        slider.ValueChanged += (_, e) => Set("dictation.fontSize", Math.Round(e.NewValue, 1));
        return slider;
    }

    private async Task FillMicrophones(ComboBox combo)
    {
        var version = pageVersion;
        var reply = await app.Engine.CallAsync("devices");
        if (settingsClosed || version != pageVersion) return;
        if (reply["microphones"] is not JsonArray list)
        {
            return;
        }
        var known = combo.Items.OfType<ComboBoxItem>().Select(i => i.Tag as string).ToHashSet();
        foreach (var name in list.Select(n => n.Str()).Where(n => n.Length > 0 && !known.Contains(n)))
        {
            combo.Items.Add(new ComboBoxItem { Content = name, Tag = name });
        }
    }

    // ---- 字幕 -------------------------------------------------------------------------

    private void BuildCaptions()
    {
        var version = pageVersion;
        var displayedOn = app.CaptionsOn;
        var on = new ToggleSwitch { IsOn = displayedOn, MinWidth = 0 };
        on.Toggled += (_, _) =>
        {
            // Remember the rendered value, rather than a short-lived syncing flag:
            // template/state callbacks can be delivered after the assignment returns.
            if (!settingsClosed && version == pageVersion && on.IsOn != displayedOn)
            {
                displayedOn = on.IsOn;
                _ = app.Engine.CallAsync("captions.set", new JsonObject { ["on"] = on.IsOn });
            }
        };
        pageEvents = (name, _) =>
        {
            if (name == "state")
            {
                displayedOn = app.CaptionsOn;
                on.IsOn = displayedOn;
            }
        };
        Card(T("cap.on.title"), T("cap.on.desc"), on, "");
        Block(T("cap.source.title"), T("cap.source.desc"), Radios("captions.source",
            ("system", T("source.system")),
            ("microphone", T("source.microphone")),
            ("mixed", T("source.mixed"))));
        Card(T("cap.lines.title"), null, Number("captions.lines", 1, 4));
        Card(T("cap.font.title"), null, SliderFor("captions.fontSize", 12, 64, 1, false));
        Card(T("cap.width.title"), null, SliderFor("captions.widthPercent", 30, 100, 5, true));
        Card(T("cap.clickThrough.title"), T("cap.clickThrough.desc"), Toggle("captions.clickThrough"));
    }

    // ---- 快捷键 -----------------------------------------------------------------------------

    private void BuildShortcuts()
    {
        var version = pageVersion;
        var rows = new Dictionary<string, (TextBlock Key, TextBlock Status)>();
        foreach (var field in new[] { "hold", "toggle", "captions", "note", "cards" })
        {
            var key = new TextBlock
            {
                Text = Theme.Keys(s.Str("hotkeys." + field)),
                Style = Res<Style>("BodyStrongTextBlockStyle"),
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(0, 0, 8, 0),
            };
            var status = Note();
            var change = new Button { Content = T("key.change") };
            var clear = new Button { Content = T("key.clear") };
            var f = field;
            change.Click += async (_, _) =>
            {
                // Calls run on the thread pool: await the stop so it cannot overtake the start.
                await StopRecordingAsync();
                var reply = await app.Engine.CallAsync("hotkey.record", new JsonObject { ["on"] = true });
                if (settingsClosed || version != pageVersion)
                {
                    // The page went away meanwhile; never leave the keyboard being recorded.
                    if (reply.Ok()) _ = app.Engine.CallAsync("hotkey.record", new JsonObject { ["on"] = false });
                    return;
                }
                if (!reply.Ok())
                {
                    status.Text = Loc.Error(reply["error"].Str());
                    return;
                }
                recordingField = f;
                status.Text = T("key.recording");
            };
            clear.Click += (_, _) =>
            {
                _ = StopRecordingAsync();
                Set("hotkeys." + f, "");
                key.Text = Theme.Keys("");
                status.Text = "";
            };
            Card(T($"key.{field}.title"), T($"key.{field}.desc"), Horizontal(key, change, clear), null, out _, status);
            rows[field] = (key, status);
        }

        pageEvents = (name, e) =>
        {
            if (name != "hotkey.recorded" || recordingField is not { } field)
            {
                return;
            }
            recordingField = null;
            var (key, status) = rows[field];
            var binding = e["binding"].Str();
            if (binding.Length == 0)
            {
                status.Text = "";
                return;
            }
            _ = SaveRecorded(field, binding, key, status);
        };
        pageClosing = () => _ = StopRecordingAsync();

        async Task SaveRecorded(string field, string binding, TextBlock key, TextBlock status)
        {
            var check = await app.Engine.CallAsync("hotkey.check", new JsonObject { ["binding"] = binding, ["field"] = field });
            if (settingsClosed || version != pageVersion) return;
            var conflict = check["conflict"].Str();
            if (!check["valid"].Bool())
            {
                status.Text = conflict == "modifier_only" ? T("key.conflict.modifier_only") : T("key.invalid");
                return;
            }
            if (conflict == "duplicate")
            {
                status.Text = T("key.conflict.duplicate");
                return;
            }
            var display = check["display"].Str();
            Set("hotkeys." + field, display);
            key.Text = Theme.Keys(display);
            status.Text = conflict == "in_use" ? T("key.conflict.in_use") : Loc.F("key.saved", Theme.Keys(display));
        }
    }

    private Task StopRecordingAsync()
    {
        if (recordingField is null)
        {
            return Task.CompletedTask;
        }
        recordingField = null;
        return app.Engine.CallAsync("hotkey.record", new JsonObject { ["on"] = false });
    }

    // ---- 服务 -------------------------------------------------------------------------------

    private ContentDialog? serviceDialog;

    /// <summary>Builds the services page once the provider list arrives, without blocking the window.</summary>
    private async Task BuildServicesAsync(int version, double offset)
    {
        var reply = await app.Engine.CallAsync("providers");
        if (settingsClosed || version != pageVersion) return;
        BuildServices(reply);
        page.UpdateLayout();
        scroller.ChangeView(null, offset, null, true);
    }

    private void BuildServices(JsonObject reply)
    {
        var version = pageVersion;
        var providers = (reply["providers"] as JsonArray)?.OfType<JsonObject>().ToList() ?? [];
        Card(T("svc.speech.title"), T("svc.speech.desc"),
            Combo("services.speech", providers.Select(p => (p["id"].Str(), ProviderName(p))),
                changed: selected => { _ = s.SaveNowAsync(); }), "");
        var statusById = new Dictionary<string, TextBlock>();
        var officialIds = providers.Where(p => p["official"].Bool()).Select(p => p["id"].Str()).ToHashSet();
        foreach (var provider in providers)
        {
            var id = provider["id"].Str();
            var caps = Capabilities(provider["capabilities"] as JsonObject);
            var official = provider["official"].Bool();
            var desc = !provider["available"].Bool() ? Loc.F("svc.unavailable", Loc.Error(provider["error"].Str()))
                : official ? T("svc.official.ready") : caps.Length > 0 ? Loc.F("svc.check.ok", caps) : T("svc.ready");
            var check = new Button { Content = T(official ? "svc.official.check" : "svc.check") };
            var card = Card(ProviderName(provider), desc, check, null, out var block);
            if (!provider["builtin"].Bool())
            {
                var menu = new MenuFlyout();
                var edit = new MenuFlyoutItem { Text = T("svc.official.edit") };
                edit.Click += async (_, _) =>
                {
                    var item = (s.Get("services.customSpeech") as JsonArray)?.OfType<JsonObject>()
                        .FirstOrDefault(item => item["id"].Str() == id);
                    if (item is not null) await ShowServiceDialog((JsonObject)item.DeepClone(), false);
                };
                menu.Items.Add(edit);
                card.ContextFlyout = menu;
            }
            statusById[id] = block;
            check.Click += async (_, _) =>
            {
                block.Text = T("svc.checking");
                block.Visibility = Visibility.Visible;
                var reply = await app.Engine.CallAsync("provider.check", new JsonObject { ["id"] = id });
                if (settingsClosed || version != pageVersion) return;
                if (!reply.Ok() || !reply["started"].Bool())
                {
                    block.Text = Loc.F("svc.check.fail", Loc.Error(reply["error"].Str()));
                }
            };
        }

        var providerType = new ComboBox { Width = 250, Header = T("svc.official.type") };
        foreach (var kind in new[] { "doubao", "openai", "qwen", "tencent" })
            providerType.Items.Add(new ComboBoxItem { Content = T("svc.official." + kind), Tag = kind });
        providerType.SelectedIndex = 0;
        var add = ActionButton(T("svc.custom.add"), async () =>
        {
            if (providerType.SelectedItem is ComboBoxItem { Tag: string kind })
                await ShowServiceDialog(NewServiceDraft(kind), true);
        });
        add.VerticalAlignment = VerticalAlignment.Bottom;
        var addRow = Horizontal(providerType, add);
        addRow.Margin = new Thickness(0, 12, 0, 0);
        page.Children.Add(addRow);

        Section(T("nav.general"));
        Card(T("svc.idle.title"), T("svc.idle.desc"), Number("services.idleReleaseMinutes", 1, 120));
        Card(T("svc.data.title"), T("svc.data.desc"), null, "");

        pageEvents = (name, e) =>
        {
            if (name == "provider.checked" && statusById.TryGetValue(e["id"].Str(), out var block))
            {
                block.Text = e["ok"].Bool()
                    ? officialIds.Contains(e["id"].Str()) ? T("svc.official.checked") : Loc.F("svc.check.ok", Capabilities(e["capabilities"] as JsonObject))
                    : Loc.F("svc.check.fail", Loc.Error(e["error"].Str()));
            }
        };
    }

    private async Task ShowServiceDialog(JsonObject draft, bool isNew)
    {
        if (settingsClosed || serviceDialog is not null || root.XamlRoot is null) return;
        var id = draft["id"].Str();
        var kind = draft["providerType"].Str("command");
        var official = kind is "doubao" or "openai" or "qwen" or "tencent";
        var secretName = "speech." + id;
        var secretReply = await app.Engine.CallAsync("secret.status", new JsonObject { ["names"] = new JsonArray(secretName) });
        // A second click may have opened the dialog while this one waited.
        if (settingsClosed || serviceDialog is not null || root.XamlRoot is null) return;
        var hasSecret = secretReply["secrets"]?[secretName].Bool() ?? false;
        // Work only on this draft; cancelled dialogs must never reach automatic settings saves.
        var panel = new StackPanel { Spacing = 12, MinWidth = 360 };
        var editorContent = new ContentControl { Content = panel, HorizontalContentAlignment = HorizontalAlignment.Stretch };
        var fields = new Dictionary<string, PasswordBox>();
        var textFields = new Dictionary<string, TextBox>();
        var status = Note(hasSecret ? T("svc.key.saved") : T("svc.key.none"));
        var busy = false;
        var changed = false;
        var saved = false;
        ComboBox? transcriptionModel = null;
        var dialog = new ContentDialog
        {
            XamlRoot = root.XamlRoot,
            RequestedTheme = root.ActualTheme,
            Title = isNew ? Loc.F("svc.official.addTitle", T("svc.official." + kind)) : T("svc.official.edit"),
            Content = new ScrollViewer { Content = editorContent, MaxHeight = 480, HorizontalScrollMode = ScrollMode.Disabled },
            PrimaryButtonText = official ? T("common.save") : "",
            SecondaryButtonText = isNew ? "" : T("svc.official.removeService"),
            CloseButtonText = T("common.cancel"),
            DefaultButton = ContentDialogButton.Primary,
        };
        serviceDialog = dialog;
        void Field(string key, string header)
        {
            var box = new TextBox { Header = T(header), Text = draft[key].Str(), HorizontalAlignment = HorizontalAlignment.Stretch };
            textFields.Add(key, box);
            panel.Children.Add(box);
        }
        if (official)
        {
            Field("name", "svc.custom.name");
            if (kind == "openai")
            {
                transcriptionModel = new ComboBox
                {
                    Header = T("svc.official.model"), IsEditable = true,
                    HorizontalAlignment = HorizontalAlignment.Stretch,
                };
                foreach (var model in new[] { "gpt-live-transcribe", "gpt-transcribe", "gpt-4o-mini-transcribe", "gpt-4o-transcribe" })
                    transcriptionModel.Items.Add(model);
                // Editable ComboBox clears Text while its items/selection initialize.
                // Select after populating, including a saved custom model name.
                var savedModel = draft["model"].Str();
                if (savedModel.Length > 0 && !transcriptionModel.Items.Contains(savedModel))
                    transcriptionModel.Items.Add(savedModel);
                transcriptionModel.SelectedItem = savedModel;
                transcriptionModel.Text = savedModel;
                panel.Children.Add(transcriptionModel);
            }
            else if (kind != "doubao") Field("model", "svc.official.model");
            if (kind == "doubao") Field("resourceId", "svc.official.resourceId");
            if (kind == "tencent") Field("appId", "svc.official.appId");
            if (kind == "openai") Field("language", "svc.official.language");
            if (kind == "qwen")
            {
                var region = new ComboBox { Header = T("svc.official.region"), HorizontalAlignment = HorizontalAlignment.Stretch };
                var endpoints = new[] { "wss://dashscope.aliyuncs.com/api-ws/v1/realtime", "wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime" };
                region.Items.Add(new ComboBoxItem { Content = T("svc.official.region.cn"), Tag = endpoints[0] });
                region.Items.Add(new ComboBoxItem { Content = T("svc.official.region.intl"), Tag = endpoints[1] });
                region.SelectedIndex = Math.Max(0, Array.IndexOf(endpoints, draft["endpoint"].Str()));
                region.SelectionChanged += (_, _) =>
                {
                    if (region.SelectedItem is ComboBoxItem { Tag: string endpoint }) draft["endpoint"] = endpoint;
                };
                panel.Children.Add(region);
            }
            foreach (var key in kind == "tencent" ? new[] { "secretId", "secretKey" } : new[] { "apiKey" })
            {
                var box = new PasswordBox { Header = T("svc.official." + key), HorizontalAlignment = HorizontalAlignment.Stretch };
                fields.Add(key, box);
                panel.Children.Add(box);
            }
            panel.Children.Add(Note(T(isNew ? "svc.official.credentials" : "svc.official.credentialsEdit")));
            if (!isNew)
            {
                var deleteKey = ActionButton(T("svc.key.delete"), async () =>
                {
                    if (busy) return;
                    SetBusy(true);
                    try
                    {
                        var reply = await app.Engine.CallAsync("secret.delete", new JsonObject { ["name"] = secretName });
                        if (reply.Ok())
                        {
                            hasSecret = false;
                            changed = true;
                            foreach (var box in fields.Values) box.Password = "";
                        }
                        status.Text = reply.Ok() ? T("svc.key.none") : Loc.Error(reply["error"].Str());
                    }
                    finally { SetBusy(false); }
                });
                panel.Children.Add(deleteKey);
            }
        }
        else
        {
            panel.Children.Add(new TextBlock { Text = draft["name"].Str(id), TextWrapping = TextWrapping.Wrap });
            panel.Children.Add(Note(T("svc.official.legacy")));
        }
        panel.Children.Add(status);
        void SetBusy(bool value)
        {
            busy = value;
            editorContent.IsEnabled = !value;
            dialog.IsPrimaryButtonEnabled = !value;
            dialog.IsSecondaryButtonEnabled = !value;
        }
        dialog.Closing += (_, e) => e.Cancel = busy;
        dialog.PrimaryButtonClick += async (_, e) =>
        {
            e.Cancel = true;
            var deferral = e.GetDeferral();
            SetBusy(true);
            try
            {
                foreach (var (key, box) in textFields) draft[key] = box.Text.Trim();
                if (transcriptionModel is not null) draft["model"] = transcriptionModel.Text.Trim();
                if (string.IsNullOrWhiteSpace(draft["name"].Str()) || (kind != "doubao" && string.IsNullOrWhiteSpace(draft["model"].Str()))
                    || (kind == "doubao" && string.IsNullOrWhiteSpace(draft["resourceId"].Str()))
                    || (kind == "tencent" && (draft["appId"].Str().Length == 0 || !draft["appId"].Str().All(char.IsAsciiDigit))))
                {
                    status.Text = T("svc.official.configRequired");
                    return;
                }
                var replacing = fields.Values.Any(box => box.Password.Length > 0);
                if ((isNew && !hasSecret) || replacing)
                {
                    if (fields.Values.Any(box => string.IsNullOrWhiteSpace(box.Password)))
                    {
                        status.Text = T("svc.official.required");
                        return;
                    }
                    var credential = new JsonObject();
                    foreach (var (key, box) in fields) credential[key] = box.Password.Trim();
                    var reply = await app.Engine.CallAsync("secret.set", new JsonObject { ["name"] = secretName, ["value"] = credential.ToJsonString() });
                    if (!reply.Ok())
                    {
                        status.Text = Loc.Error(reply["error"].Str());
                        return;
                    }
                    hasSecret = true;
                    changed = true;
                    foreach (var box in fields.Values) box.Password = "";
                }
                var error = await SaveServiceConfiguration(draft, id);
                if (error is not null)
                {
                    status.Text = hasSecret ? Loc.F("svc.official.configSaveFailed", error) : error;
                    return;
                }
                saved = changed = true;
                e.Cancel = false;
            }
            finally { SetBusy(false); deferral.Complete(); }
        };
        dialog.SecondaryButtonClick += async (_, e) =>
        {
            e.Cancel = true;
            var deferral = e.GetDeferral();
            SetBusy(true);
            try
            {
                var error = await SaveServiceConfiguration(null, id);
                if (error is not null) { status.Text = error; return; }
                changed = true;
                var reply = await app.Engine.CallAsync("secret.delete", new JsonObject { ["name"] = secretName });
                if (!reply.Ok()) ShowIssue(Loc.Error(reply["error"].Str()));
                e.Cancel = false;
            }
            finally { SetBusy(false); deferral.Complete(); }
        };
        try { await dialog.ShowAsync(); }
        finally
        {
            foreach (var box in fields.Values) box.Password = "";
            // A failed new-provider save may have stored its key. Do not leave an orphan on cancel.
            if (isNew && !saved && hasSecret)
            {
                var reply = await app.Engine.CallAsync("secret.delete", new JsonObject { ["name"] = secretName });
                if (!reply.Ok()) ShowIssue(Loc.Error(reply["error"].Str()));
            }
            serviceDialog = null;
            if (!settingsClosed && changed && current == "services") ShowPage(current, keepScroll: true);
        }
    }

    private static JsonObject NewServiceDraft(string kind)
    {
        var (model, endpoint) = kind switch
        {
            "openai" => ("gpt-live-transcribe", "wss://api.openai.com/v1/realtime?intent=transcription"),
            "qwen" => ("qwen3-asr-flash-realtime", "wss://dashscope.aliyuncs.com/api-ws/v1/realtime"),
            "tencent" => ("16k_zh", "wss://asr.cloud.tencent.com/asr/v2/"),
            _ => ("bigmodel", "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async"),
        };
        return new JsonObject
        {
            ["id"] = "official-" + Guid.NewGuid().ToString("N")[..8],
            ["name"] = T("svc.official." + kind), ["providerType"] = kind,
            ["model"] = model, ["endpoint"] = endpoint, ["appId"] = "",
            ["resourceId"] = kind == "doubao" ? "volc.bigasr.sauc.duration" : "", ["language"] = "zh",
        };
    }

    private async Task<string?> SaveServiceConfiguration(JsonObject? draft, string id)
    {
        // Flush unrelated edits first, then send the draft directly. A failed save must not
        // leave a new service in SettingsStore's automatic retry queue.
        if (!await s.SaveNowAsync()) return Loc.Error("save_failed");
        var settings = (JsonObject)s.Root.DeepClone();
        var services = settings["services"] as JsonObject ?? new JsonObject();
        if (settings["services"] is null) settings["services"] = services;
        var list = services["customSpeech"] as JsonArray ?? new JsonArray();
        if (services["customSpeech"] is null) services["customSpeech"] = list;
        var existing = list.OfType<JsonObject>().FirstOrDefault(item => item["id"].Str() == id);
        if (draft is null)
        {
            if (existing is not null) list.Remove(existing);
            if (services["speech"].Str() == id) services["speech"] = "doubao";
        }
        else if (existing is not null) list[list.IndexOf(existing)] = draft.DeepClone();
        else list.Add(draft.DeepClone());
        var reply = await app.Engine.CallAsync("settings.set", new JsonObject { ["settings"] = settings });
        if (!reply.Ok()) return Loc.Error(reply["error"].Str());
        // Keep unrelated changes made while IPC was pending (for example bar scaling).
        // The normal save queue subsequently merges this successful service update.
        Set("services", (reply["settings"]?["services"] ?? services).DeepClone());
        return null;
    }

    private static string ProviderName(JsonObject provider) =>
        provider["builtin"].Bool() ? T("svc.doubao") : provider["name"].Str(provider["id"].Str());

    private static string Capabilities(JsonObject? caps)
    {
        if (caps is null)
        {
            return "";
        }
        if (caps["interim"].Bool() && caps["final"].Bool()) return T("caps.dictation");
        var parts = new[] { "interim", "final", "timestamps" }
            .Where(k => caps[k].Bool())
            .Select(k => T("caps." + k));
        return string.Join(Loc.English ? ", " : "、", parts);
    }

    // ---- 通用 -------------------------------------------------------------------------------

    private void BuildGeneral()
    {
        Card(T("gen.startup.title"), null, Toggle("general.startWithWindows"), "");
        var diagStatus = Note();
        var export = ActionButton(T("gen.diag.export"), async () =>
        {
            var reply = await app.Engine.CallAsync("diagnostics.export");
            if (reply.Ok())
            {
                var path = reply["path"].Str();
                diagStatus.Text = Loc.F("gen.diag.done", path);
                app.OpenFolder(Path.GetDirectoryName(path) ?? app.ConfigDir);
            }
            else
            {
                diagStatus.Text = Loc.Error(reply["error"].Str());
            }
        });
        var logs = ActionButton(T("gen.logs"), () =>
            app.OpenFolder(string.IsNullOrEmpty(app.LogFile) ? app.ConfigDir : Path.GetDirectoryName(app.LogFile) ?? app.ConfigDir));
        Card(T("gen.diag.title"), null, Horizontal(export, logs), "", out _, diagStatus);
        var update = new Button { Content = T("gen.update"), MinWidth = 88 };
        update.Click += async (_, _) =>
        {
            update.IsEnabled = false;
            try { await UpdateService.CheckAsync(this); }
            finally { if (!settingsClosed) update.IsEnabled = true; }
        };
        Card(T("app.name"), Loc.F("gen.version", EngineHost.Version), update, "");
        page.Children.Add(new TextBlock
        {
            Text = "Copyright © 2026 FueTsui. All Rights Reserved.",
            Style = Res<Style>("SecondaryTextStyle"), TextAlignment = TextAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Stretch, Margin = new Thickness(0, 24, 0, 0),
        });
    }
}
