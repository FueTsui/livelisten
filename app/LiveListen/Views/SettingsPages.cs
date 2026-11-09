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
    private sealed class ShortcutRow(string field, HotkeyInput input, TextBlock status, Button retry)
    {
        public string Field { get; } = field;
        public HotkeyInput Input { get; } = input;
        public TextBlock Status { get; } = status;
        public Button Retry { get; } = retry;
        public string? Draft { get; set; }
    }
    private sealed class ShortcutDraft(string binding)
    {
        public string Binding { get; } = binding;
        public string Message { get; set; } = T("key.saving");
        public bool Saving { get; set; } = true;
    }
    private readonly Dictionary<string, ShortcutDraft> shortcutDrafts = [];
    private readonly SemaphoreSlim hotkeyOperations = new(1, 1);
    private bool shortcutSettingsSaving;
    private ShortcutRow? recordingRow;
    private ulong? captureToken;
    private long captureRevision;
    private Action? refreshShortcutValues;
    private Action? afterOwnChange;

    private static string T(string key) => Loc.T(key);

    // ---- 听写 -----------------------------------------------------------------------------

    private void BuildDictation()
    {
        var mic = Combo("dictation.microphone", [("", T("dict.mic.default"))], 280);
        Card(T("dict.mic.title"), null, mic, "");
        _ = FillMicrophones(mic);
        resumePage = () => _ = FillMicrophones(mic);
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
        var devices = list.Select(n => n.Str()).Where(n => n.Length > 0).ToHashSet();
        foreach (var item in combo.Items.OfType<ComboBoxItem>().ToArray())
        {
            var name = (string)item.Tag;
            if (name.Length == 0 || devices.Remove(name) || ReferenceEquals(item, combo.SelectedItem)) continue;
            combo.Items.Remove(item);
        }
        foreach (var name in devices)
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
        var rows = new List<ShortcutRow>();
        foreach (var field in new[] { "hold", "toggle", "captions", "note", "noteToggle", "noteBlank", "cards" })
        {
            var input = new HotkeyInput(T($"key.{field}.title"), s.Str("hotkeys." + field));
            var retry = new Button { Content = T("key.retry"), Visibility = Visibility.Collapsed, Padding = new Thickness(8, 2, 8, 2), MinHeight = 24 };
            Card(T($"key.{field}.title"), T($"key.{field}.desc"), input.View, null, out var status, retry);
            var row = new ShortcutRow(field, input, status, retry);
            rows.Add(row);
            input.Requested += () => _ = StartRecordingAsync(row, version);
            input.Cancelled += () => { if (recordingRow == row) _ = StopRecordingAsync(); };
            input.Committed += binding => _ = SaveShortcutAsync(row, binding, version);
            input.ClearRequested += () => _ = SaveShortcutAsync(row, "", version);
            input.Unsupported += () => status.Text = T("key.invalid");
            retry.Click += (_, _) => { if (row.Draft is { } draft) _ = SaveShortcutAsync(row, draft, version); };
        }
        refreshShortcutValues = () =>
        {
            foreach (var row in rows)
            {
                if (row.Input.IsCapturing) continue;
                if (shortcutDrafts.TryGetValue(row.Field, out var draft))
                {
                    row.Draft = draft.Binding;
                    row.Input.SetValue(draft.Binding);
                    row.Input.SetBusy(draft.Saving);
                    row.Status.Text = draft.Message;
                    row.Retry.Visibility = draft.Saving ? Visibility.Collapsed : Visibility.Visible;
                }
                else
                {
                    var wasPending = row.Input.IsBusy || row.Draft is not null;
                    row.Input.SetBusy(false);
                    row.Draft = null;
                    row.Input.SetValue(s.Str("hotkeys." + row.Field));
                    row.Retry.Visibility = Visibility.Collapsed;
                    if (wasPending) row.Status.Text = T($"key.{row.Field}.desc");
                }
            }
        };
        refreshShortcutValues();
        pageClosing = () => _ = StopRecordingAsync();
    }

    private async Task StartRecordingAsync(ShortcutRow row, int version)
    {
        var request = ++captureRevision;
        if (recordingRow is { } previous) { previous.Input.Stop(); previous.Status.Text = previous.Draft is null ? T($"key.{previous.Field}.desc") : T("key.unsaved"); }
        recordingRow = row;
        row.Input.Prepare();
        row.Status.Text = T("key.preparing");
        await hotkeyOperations.WaitAsync();
        try
        {
            await ReleaseCaptureAsync();
            if (settingsClosed || version != pageVersion || request != captureRevision || !row.Input.HasInputFocus) return;
            var reply = await app.Engine.CallAsync("hotkey.capture", new JsonObject
            {
                ["on"] = true, ["window"] = Overlay.Handle(this).ToInt64(),
            });
            if (!reply.Ok() || !reply["active"].Bool() || reply["token"] is null)
            {
                row.Status.Text = Loc.Error(reply["error"].Str("capture_unavailable"));
                return;
            }
            captureToken = reply["token"]!.GetValue<ulong>();
            if (settingsClosed || version != pageVersion || request != captureRevision || !row.Input.HasInputFocus)
            {
                await ReleaseCaptureAsync();
                return;
            }
            row.Input.Begin();
            row.Status.Text = T("key.recording");
        }
        catch (Exception) { if (!settingsClosed && version == pageVersion) row.Status.Text = Loc.Error("capture_unavailable"); }
        finally
        {
            if (request == captureRevision && captureToken is null) { row.Input.Stop(); recordingRow = null; }
            hotkeyOperations.Release();
        }
    }

    private async Task StopRecordingAsync()
    {
        captureRevision++;
        var stoppedRow = recordingRow;
        if (recordingRow is { } row)
        {
            row.Input.Stop();
            row.Status.Text = row.Draft is null ? T($"key.{row.Field}.desc") : T("key.unsaved");
            recordingRow = null;
        }
        await hotkeyOperations.WaitAsync();
        try { await ReleaseCaptureAsync(); }
        catch (Exception)
        {
            if (!settingsClosed && stoppedRow is not null)
                stoppedRow.Status.Text = Loc.Error("capture_unavailable");
        }
        finally { hotkeyOperations.Release(); }
    }

    private async Task ReleaseCaptureAsync()
    {
        if (captureToken is not { } token) return;
        captureToken = null;
        var reply = await app.Engine.CallAsync("hotkey.capture", new JsonObject { ["on"] = false, ["token"] = token });
        if (!reply.Ok()) throw new InvalidOperationException("capture_unavailable");
    }

    private async Task SaveShortcutAsync(ShortcutRow row, string binding, int version)
    {
        if (row.Input.IsBusy) return;
        captureRevision++;
        if (recordingRow is { } active) { active.Input.Stop(); recordingRow = null; }
        row.Input.Stop();
        var draft = new ShortcutDraft(binding);
        shortcutDrafts[row.Field] = draft;
        row.Draft = binding;
        row.Input.SetValue(binding);
        row.Input.SetBusy(true);
        row.Retry.Visibility = Visibility.Collapsed;
        row.Status.Text = T("key.saving");
        await hotkeyOperations.WaitAsync();
        try
        {
            await ReleaseCaptureAsync();
            if (settingsClosed || version != pageVersion) return;
            var canonical = HotkeyCaptureState.Canonical(binding);
            if (canonical is null) { row.Status.Text = T("key.invalid"); return; }
            bool Duplicate(string value) => s.Root["hotkeys"] is JsonObject all
                && HotkeyCaptureState.HasDuplicate(value, row.Field, all.Select(pair => new KeyValuePair<string, string>(pair.Key, pair.Value.Str())));
            if (Duplicate(canonical)) { row.Status.Text = T("key.conflict.duplicate"); return; }
            var conflict = "";
            if (canonical.Length > 0)
            {
                var check = await app.Engine.CallAsync("hotkey.check", new JsonObject { ["binding"] = canonical, ["field"] = row.Field });
                if (settingsClosed || version != pageVersion) return;
                conflict = check["conflict"].Str();
                if (!check["valid"].Bool())
                {
                    row.Status.Text = conflict == "modifier_only" ? T("key.conflict.modifier_only") : check["error"] is not null ? Loc.Error(check["error"].Str()) : T("key.invalid");
                    return;
                }
                if (conflict == "duplicate" || Duplicate(canonical)) { row.Status.Text = T("key.conflict.duplicate"); return; }
                canonical = check["display"].Str(canonical);
            }
            shortcutSettingsSaving = true;
            try
            {
                Set("hotkeys." + row.Field, canonical);
                if (!await s.SaveNowAsync()) { row.Status.Text = T("key.unsaved"); return; }
            }
            finally { shortcutSettingsSaving = false; }
            if (HotkeyCaptureState.Canonical(s.Str("hotkeys." + row.Field)) != HotkeyCaptureState.Canonical(canonical))
            {
                row.Status.Text = T("key.invalid");
                return;
            }
            ClearSavedSettingsIssue();
            row.Draft = null;
            row.Input.SetValue(canonical);
            row.Status.Text = (canonical.Length == 0 ? T("key.cleared") : Loc.F("key.saved", Theme.Keys(canonical)))
                + (conflict == "in_use" ? " · " + T("key.conflict.in_use") : "");
        }
        catch (Exception) { row.Status.Text = T("key.unsaved"); }
        finally
        {
            row.Input.SetBusy(false);
            row.Retry.Visibility = row.Draft is null ? Visibility.Collapsed : Visibility.Visible;
            if (row.Draft is not null && row.Status.Text != T("key.unsaved"))
                row.Status.Text = Loc.F("key.notSavedReason", row.Status.Text);
            if (shortcutDrafts.TryGetValue(row.Field, out var latest) && ReferenceEquals(latest, draft))
            {
                if (row.Draft is null) shortcutDrafts.Remove(row.Field);
                else { draft.Saving = false; draft.Message = row.Status.Text; }
            }
            hotkeyOperations.Release();
            if (!settingsClosed) refreshShortcutValues?.Invoke();
        }
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
        var version = pageVersion;
        var id = draft["id"].Str();
        var kind = draft["providerType"].Str("command");
        var official = kind is "doubao" or "openai" or "qwen" or "tencent";
        var secretName = "speech." + id;
        var secretReply = await app.Engine.CallAsync("secret.status", new JsonObject { ["names"] = new JsonArray(secretName) });
        // A second click may have opened the dialog while this one waited.
        if (settingsClosed || pageVersion != version || serviceDialog is not null || root.XamlRoot is null) return;
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
            if (!settingsClosed && changed && pageVersion == version) ShowPage(current, keepScroll: true);
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
        diagStatus.Visibility = Visibility.Collapsed;
        var export = ActionButton(T("gen.diag.export"), async () =>
        {
            var exportPageVersion = pageVersion;
            var reply = await app.Engine.CallAsync("diagnostics.export");
            var path = reply.Ok() ? reply["path"].Str() : null;
            if (!settingsClosed && pageVersion == exportPageVersion)
            {
                diagStatus.Text = path is null ? Loc.Error(reply["error"].Str()) : Loc.F("gen.diag.done", path);
                diagStatus.Visibility = Visibility.Visible;
            }
            if (path is not null) app.OpenFolder(Path.GetDirectoryName(path) ?? app.ConfigDir);
        });
        var logs = ActionButton(T("gen.logs"), () =>
            app.OpenFolder(string.IsNullOrEmpty(app.LogFile) ? app.ConfigDir : Path.GetDirectoryName(app.LogFile) ?? app.ConfigDir));
        Card(T("gen.diag.title"), null, Horizontal(export, logs), "", out _, footer: diagStatus);
        var update = new Button { Content = T("gen.update"), MinWidth = 88 };
        var downloads = new Button { Content = T("gen.update.open"), Visibility = Visibility.Collapsed };
        var progress = new ProgressBar { Minimum = 0, Maximum = 100, Height = 3, Margin = new Thickness(0, 8, 0, 0), Visibility = Visibility.Collapsed };
        Card(T("app.name"), Loc.F("gen.version", EngineHost.Version), Horizontal(update, downloads), "", out var versionText, progress);
        var updater = UpdateService.Shared;
        var alive = true;
        void Render(UpdateStatus status)
        {
            if (!alive || settingsClosed) return;
            var version = Loc.F("gen.version", EngineHost.Version);
            var detail = status.Phase switch
            {
                UpdatePhase.Checking => T("gen.update.checking"),
                UpdatePhase.Current => T("gen.update.current"),
                UpdatePhase.Downloading => Loc.F("gen.update.downloading", status.Version!, status.Percent),
                UpdatePhase.Verifying => Loc.F("gen.update.verifying", status.Version!),
                UpdatePhase.Ready => Loc.F("gen.update.ready", status.Version!),
                UpdatePhase.Failed => (status.Version is null ? "" : Loc.F("gen.version", status.Version) + " · ") + T("gen.update.error." + status.Error),
                _ => "",
            };
            versionText.Text = string.IsNullOrEmpty(detail) ? version : version + " · " + detail;
            update.IsEnabled = status.Phase is not (UpdatePhase.Checking or UpdatePhase.Downloading or UpdatePhase.Verifying);
            update.Content = T(status.Phase == UpdatePhase.Failed ? "gen.update.retry" : "gen.update");
            progress.Visibility = update.IsEnabled ? Visibility.Collapsed : Visibility.Visible;
            progress.IsIndeterminate = status.Phase is UpdatePhase.Checking or UpdatePhase.Verifying;
            progress.Value = status.Percent;
            downloads.Visibility = status.Phase == UpdatePhase.Ready ? Visibility.Visible : Visibility.Collapsed;
        }
        void Updated(UpdateStatus status)
        {
            if (!settingsClosed) DispatcherQueue.TryEnqueue(() => Render(status));
        }
        updater.Changed += Updated;
        pageClosing += () => { alive = false; updater.Changed -= Updated; };
        resumePage = () =>
        {
            diagStatus.Text = "";
            diagStatus.Visibility = Visibility.Collapsed;
            Render(updater.Status);
        };
        Render(updater.Status);
        update.Click += async (_, _) =>
        {
            var directory = Path.Combine(EngineHost.TestConfigDir ?? Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "LiveListen"), "Updates");
            await updater.CheckAsync(EngineHost.Version, directory, File.Exists(Path.Combine(AppContext.BaseDirectory, "Uninstall.exe")));
        };
        downloads.Click += (_, _) =>
        {
            if (updater.Status.DownloadPath is { } path && Path.GetDirectoryName(path) is { } directory)
                app.OpenFolder(directory);
        };
    }
}
