using System.Diagnostics;
using System.Text.Json.Nodes;
using LiveListen.Interop;
using LiveListen.Services;
using LiveListen.Views;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace LiveListen;

public partial class App : Application
{
    private readonly bool background;
    private readonly bool openSettings;
    private readonly string? requestedVoiceStyle;
    private DispatcherQueue dispatcher = null!;
    private TrayIcon? tray;
    private OverlayContextMenu? overlayMenu;
    private CaptionsWindow? captions;
    private SettingsWindow? settingsWindow;
    private volatile bool exiting;
    private DispatcherQueueTimer? updateTimer;
    private int exitVersion;
    private long stateVersion;
    private long cardsVersion;

    public App(string[] args)
    {
        Instance = this;
        background = args.Any(a => a.Equals("--background", StringComparison.OrdinalIgnoreCase));
        openSettings = args.Any(a => a.Equals("--settings", StringComparison.OrdinalIgnoreCase));
        requestedVoiceStyle = VoiceStyleArgument(args);
        InitializeComponent();
        DispatcherShutdownMode = DispatcherShutdownMode.OnExplicitShutdown;
        UnhandledException += (_, e) =>
        {
            // A broken window must not take down the shortcuts and dictation in the tray.
            LogUi(e.Exception);
            e.Handled = !Exiting;
            // XAML may still be unwinding a failed layout/navigation callback.
            // Rendering another XAML message here can turn a recoverable exception
            // into a native reentrancy crash; the tray notification needs no layout.
            if (!Exiting) tray?.Notify(Loc.T("app.name"), Loc.T("error.ui"));
        };
    }

    public static App? Instance { get; private set; }

    internal EngineHost Engine { get; private set; } = null!;

    internal SettingsStore Settings { get; private set; } = null!;

    internal BarWindow Bar { get; private set; } = null!;

    /// <summary>Note and to-do cards shown on the desktop.</summary>
    internal CardsHost Cards { get; private set; } = null!;

    /// <summary>Latest engine state (dictation phase, captions on).</summary>
    internal JsonObject State { get; private set; } = new();

    internal bool Exiting => exiting;

    internal string ConfigDir { get; private set; } = "";

    internal string LogFile { get; private set; } = "";

    internal bool Dictating => (State["dictation"] as JsonObject)?["phase"].Str("idle") is { } p && p != "idle";

    internal bool CaptionsOn => (State["captions"] as JsonObject)?["on"].Bool() ?? false;

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        dispatcher = DispatcherQueue.GetForCurrentThread();
        var engine = EngineHost.Start(dispatcher, out var error);
        if (engine is null)
        {
            Loc.Init("system");
            Win32.MessageBox(0, Loc.Error(error ?? "core_failed"), Loc.T("app.name"), Win32.MB_ICONERROR);
            Exit();
            return;
        }
        Engine = engine;
        Settings = new SettingsStore(engine, dispatcher);
        Settings.Load();
        Loc.Init(Settings.Str("appearance.language", "system"));
        try
        {
            Microsoft.Windows.Globalization.ApplicationLanguages.PrimaryLanguageOverride = Loc.Tag;
        }
        catch (Exception)
        {
            // Not fatal: built-in control text then follows Windows.
        }

        var status = engine.Call("status");
        State = status;
        ConfigDir = status["configDir"].Str();
        LogFile = status["logFile"].Str();

        Engine.EventReceived += OnEngineEvent;
        Settings.Changed += OnSettingsChanged;
        Settings.Issues += issues =>
        {
            var text = Loc.F("issues.fixed", string.Join(", ", issues.Select(i => i.Field)));
            if (settingsWindow is { IsOpen: true })
            {
                settingsWindow.ShowIssue(text);
            }
            else
            {
                Bar.ShowMessage(text, true);
            }
        };
        Settings.SaveFailed += code =>
        {
            var text = Loc.Error(code == "stopped" ? code : "save_failed");
            if (settingsWindow is { IsOpen: true })
            {
                settingsWindow.ShowSettingsSaveFailure(code);
            }
            else
            {
                Bar.ShowMessage(text, true);
            }
        };
        Theme.SystemChanged += () => dispatcher.TryEnqueue(() => OnSettingsChanged("appearance.theme"));
        // A test run with its own settings folder never touches the user's startup entry. An
        // entry that starts exactly this program (the installer's "start at login" option)
        // turns the setting on instead of being removed.
        if (EngineHost.TestConfigDir is null)
        {
            if (!Settings.Bool("general.startWithWindows") && Startup.PointsHere())
            {
                Settings.Set("general.startWithWindows", true);
            }
            else
            {
                Startup.Sync(Settings.Bool("general.startWithWindows"));
            }
        }

        tray = new TrayIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico"), Loc.T("app.name"));
        tray.Clicked += ToggleBar;
        tray.Menu = BuildMenu;
        tray.SessionEnding += OnSessionEnding;
        tray.ScreenActiveChanged += active => { if (!exiting) Bar.SetScreenActive(active); };
        tray.DisplayChanged += () =>
        {
            if (exiting) return;
            Bar.EnsureOnScreen();
            Cards.KeepOnScreen();
        };

        Bar = new BarWindow(this);
        Cards = new CardsHost(this);
        Cards.Load();
        if (requestedVoiceStyle is not null) Settings.Set("appearance.voiceStyle", requestedVoiceStyle);
        // Started by the user: always show the bar. Started with Windows: the way it was left.
        if (!background || Settings.Bool("general.barPinned"))
        {
            Bar.Pin(true);
        }
        if (!status["hook"].Bool())
        {
            Bar.ShowMessage(Loc.Error("hook_unavailable"), true);
        }
        if (openSettings)
        {
            OpenSettings();
        }
        SyncDailyUpdateChecks();
        RemoveOldUpdatePackages();
    }

    /// <summary>
    /// Windows ends the process as soon as this returns, without the orderly exit: commit
    /// card drafts and settings and let the core write everything, all on this thread.
    /// </summary>
    private void OnSessionEnding()
    {
        if (exiting) return;
        exiting = true;
        try
        {
            Cards?.CommitDrafts();
            Bar.CommitScale();
            // Draft patches travel on pool threads; give them a moment to reach the core
            // before the shutdown command, which rejects everything after it.
            Thread.Sleep(200);
            Settings.SaveBlocking();
            Engine.Call("shutdown");
        }
        catch (Exception error)
        {
            LogUi(error);
        }
    }

    /// <summary>Installer packages of this or an older version are no longer needed.</summary>
    private static void RemoveOldUpdatePackages()
    {
        if (EngineHost.TestConfigDir is not null || !Version.TryParse(EngineHost.Version, out var current)) return;
        var folder = UpdatesDirectory;
        _ = Task.Run(() =>
        {
            try
            {
                if (!Directory.Exists(folder)) return;
                foreach (var directory in Directory.GetDirectories(folder))
                {
                    var name = Path.GetFileName(directory);
                    var dash = name.IndexOf('-');
                    if (dash <= 0 || !Version.TryParse(name[..dash], out var version) || version > current) continue;
                    try { Directory.Delete(directory, recursive: true); }
                    catch (IOException) { }
                    catch (UnauthorizedAccessException) { }
                }
            }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
        });
    }

    /// <summary>
    /// Called on a background thread when the app is launched again: <c>--quit</c> exits
    /// normally, <c>--settings</c> opens the settings window, anything else shows the bar.
    /// </summary>
    internal void OnRelaunched(string arguments) => dispatcher.TryEnqueue(() =>
    {
        if (exiting) return;
        if (arguments.Contains("--quit", StringComparison.OrdinalIgnoreCase))
        {
            Quit();
        }
        else if (arguments.Contains("--settings", StringComparison.OrdinalIgnoreCase))
        {
            OpenSettings();
        }
        else
        {
            var style = VoiceStyleArgument(arguments.Split(' ', StringSplitOptions.RemoveEmptyEntries));
            if (style is not null)
            {
                Settings.Set("appearance.voiceStyle", style);
                settingsWindow?.AppWindow.Hide();
            }
            Bar.Pin(true);
        }
    });

    private static string? VoiceStyleArgument(IEnumerable<string> arguments) => arguments
        .Select(a => a.ToLowerInvariant())
        .FirstOrDefault(a => a is "--voice-style=orb" or "--voice-style=ribbon")?
        .Split('=')[1];

    private void OnEngineEvent(string name, JsonObject e)
    {
        if (name == "state") stateVersion++;
        if (name == "cards.changed") cardsVersion++;
        if (exiting)
        {
            return;
        }
        switch (name)
        {
            case "state":
                State = e;
                Bar.OnState(e);
                ShowCaptions(CaptionsOn);
                captions?.OnState(e);
                tray?.SetTip(Dictating ? Loc.T("tray.listening") : Loc.T("app.name"));
                break;
            case "level":
                Bar.OnLevel(e["value"].Num());
                break;
            case "dictation.text":
                Bar.OnText(e["text"].Str(), e["unwritten"].Str());
                break;
            case "dictation.done":
                Bar.OnDone(e);
                break;
            case "captions.lines":
                captions?.OnLines(e["lines"] as JsonArray);
                break;
            case "notice":
                Bar.OnNotice(e["code"].Str());
                break;
            case "cards.changed":
                Cards.Apply(e);
                break;
            case "note.saved":
                Bar.ShowMessage(Loc.T(e["kind"].Str() == "todo" ? "note.saved.todo" : "note.saved.note"), false);
                break;
            case "error":
                var scope = e["scope"].Str();
                var code = e["code"].Str();
                if (scope == "captions" && captions is not null)
                {
                    captions.OnError(code);
                }
                else
                {
                    Bar.ShowMessage(Loc.Error(code), true);
                }
                break;
        }
        settingsWindow?.OnEngineEvent(name, e);
    }

    private void OnSettingsChanged(string path)
    {
        var refresh = SettingsRefresh.For(path);
        if (refresh.Bar)
        {
            Bar.Refresh();
        }
        if (path is "appearance.voiceScale" or "appearance.ribbonScale") return;
        if (refresh.Captions)
        {
            captions?.Refresh();
        }
        if (path is "general.startWithWindows" && EngineHost.TestConfigDir is null && !Startup.Apply(Settings.Bool(path)))
        {
            Bar.ShowMessage(Loc.Error("save_failed"), true);
        }
        if (refresh.Theme) settingsWindow?.ApplyTheme();
        if (refresh.Cards) Cards.Refresh();
        settingsWindow?.OnSettingsChanged(path);
        if (path is "*" or "general.checkUpdatesDaily") SyncDailyUpdateChecks();
    }

    private IReadOnlyList<TrayItem> BuildMenu() => exiting ? [] :
    [
        new(TrayShortcutLabel(Dictating ? "tray.stop" : "tray.dictate", "toggle"), ToggleDictation),
        new(TrayShortcutLabel("tray.captions", "captions"), ToggleCaptions, CaptionsOn),
        new(TrayShortcutLabel("tray.note", "noteToggle"), ToggleNote),
        TrayItem.Separator,
        new(Loc.T("tray.showDictation"), ToggleDictationText, Settings.Bool("dictation.showText")),
        new(TrayShortcutLabel("tray.cards", "cards"), ToggleCards, Cards.Shown),
        TrayItem.Separator,
        new(Loc.T("tray.resetBar"), () => { if (!exiting) Bar.ResetPosition(); }),
        new(Loc.T("tray.settings"), () => OpenSettings()),
        new(Loc.T("tray.exit"), Quit),
    ];

    private string TrayShortcutLabel(string labelKey, string shortcutField)
    {
        var label = Loc.T(labelKey);
        var shortcut = Settings.Str("hotkeys." + shortcutField);
        return string.IsNullOrWhiteSpace(shortcut) ? label : label + "\t" + Theme.Keys(shortcut);
    }

    internal void ToggleDictation() { if (!exiting) _ = Engine.CallAsync("dictation.toggle"); }

    // Starting is idempotent in the engine, including when UI state trails an event.
    internal void ShowOverlayMenu(Window owner, FrameworkElement anchor, IReadOnlyList<TrayItem> items)
    {
        if (!exiting)
        {
            overlayMenu ??= new OverlayContextMenu();
            overlayMenu.Show(owner, anchor, items, Theme.IsDark(Settings.Str("appearance.theme", "system")));
        }
    }

    internal void ToggleCaptions() { if (!exiting) _ = Engine.CallAsync("captions.set", new JsonObject { ["on"] = !CaptionsOn }); }

    internal void ToggleBar() { if (!exiting) Bar.Pin(!Bar.Pinned); }

    private void ToggleDictationText() { if (!exiting) Settings.Set("dictation.showText", !Settings.Bool("dictation.showText")); }

    internal void ToggleNote() { if (!exiting) _ = Engine.CallAsync("note.toggle"); }

    internal void ToggleCards() { if (!exiting) _ = Engine.CallAsync("cards.show", new JsonObject { ["on"] = !Cards.Shown }); }

    /// <summary>Opens the spoken-notes page of the settings window.</summary>
    internal void OpenNotebook() => OpenSettings("notebook");

    /// <summary>Opens the card appearance settings with one card selected.</summary>
    internal void OpenCardStyle(ulong id)
    {
        if (exiting) return;
        OpenSettings();
        settingsWindow!.OpenCardStyle(id);
    }

    /// <summary>First time the bar is closed, explain where the app went.</summary>
    internal void BarClosedByUser()
    {
        if (!Settings.Bool("general.closeHintShown"))
        {
            tray?.Notify(Loc.T("tray.hidden.title"), Loc.T("tray.hidden.text"));
            Settings.Set("general.closeHintShown", true);
        }
    }

    private void ShowCaptions(bool on)
    {
        if (on)
        {
            captions ??= new CaptionsWindow(this);
            captions.ShowOverlay();
        }
        else
        {
            captions?.HideOverlay();
        }
    }

    internal void OpenSettings(string? page = null)
    {
        if (exiting) return;
        settingsRelease?.Stop();
        if (settingsWindow is null)
        {
            var window = settingsWindow = new SettingsWindow(this);
            window.Closed += (_, _) => { if (ReferenceEquals(settingsWindow, window)) settingsWindow = null; };
            window.Hidden += () =>
            {
                // Reopening soon reuses the window; after a long pause its memory is returned.
                settingsRelease ??= ReleaseTimer(() =>
                {
                    if (exiting || settingsWindow is not { IsOpen: false } idle) return;
                    settingsWindow = null;
                    idle.CloseHost();
                });
                settingsRelease.Stop();
                settingsRelease.Start();
            };
        }
        settingsWindow.Open(page);
    }

    private DispatcherQueueTimer? settingsRelease;

    private DispatcherQueueTimer ReleaseTimer(Action tick)
    {
        var timer = dispatcher.CreateTimer();
        timer.Interval = TimeSpan.FromMinutes(10);
        timer.IsRepeating = false;
        timer.Tick += (_, _) => tick();
        return timer;
    }

    private static string UpdatesDirectory => Path.Combine(EngineHost.TestConfigDir ?? Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "LiveListen"), "Updates");

    private static bool IsInstalled => File.Exists(Path.Combine(AppContext.BaseDirectory, "install-record.json"))
        && File.Exists(Path.Combine(AppContext.BaseDirectory, "Uninstall.exe"));

    private void SyncDailyUpdateChecks()
    {
        // Isolated UI instances must not schedule network traffic or run an installer.
        if (EngineHost.TestConfigDir is not null || exiting) return;
        if (!Settings.Bool("general.checkUpdatesDaily"))
        {
            updateTimer?.Stop();
            return;
        }
        if (updateTimer is null)
        {
            updateTimer = dispatcher.CreateTimer();
            updateTimer.Interval = TimeSpan.FromMinutes(15);
            updateTimer.Tick += (_, _) => _ = CheckDailyUpdateAsync();
        }
        if (updateTimer.IsRunning) return;
        updateTimer.Start();
        _ = CheckDailyUpdateAsync();
    }

    private async Task CheckDailyUpdateAsync()
    {
        if (exiting || !Settings.Bool("general.checkUpdatesDaily")) return;
        var version = await UpdateService.Shared.CheckDailyAsync(EngineHost.Version, UpdatesDirectory, IsInstalled,
            DateOnly.FromDateTime(DateTime.Now));
        if (version is not null && !exiting && Settings.Bool("general.checkUpdatesDaily"))
            tray?.Notify(Loc.T("app.name"), Loc.F("gen.update.available", version));
    }

    internal Task CheckUpdateAsync()
    {
        if (exiting) return Task.CompletedTask;
        var installed = IsInstalled;
        return UpdateService.Shared.CheckAsync(EngineHost.Version, UpdatesDirectory, installed,
            install: installed && EngineHost.TestConfigDir is null ? InstallUpdateAsync : null);
    }

    private Task InstallUpdateAsync(string package, CancellationToken token) =>
        exiting ? Task.FromException(new UpdateException("install"))
            : UpdateInstaller.RunAsync(package, AppContext.BaseDirectory, token);

    internal void OpenFolder(string path)
    {
        if (Directory.Exists(path))
        {
            Process.Start(new ProcessStartInfo("explorer.exe") { ArgumentList = { path }, UseShellExecute = false });
        }
    }

    internal async void Quit()
    {
        if (exiting)
        {
            return;
        }
        exitVersion++;
        exiting = true;
        SetExitInteraction(false);
        try
        {
            // Closing settings commits a card style being edited there. Card drafts must be
            // captured while their editors still exist. The UI remains free while writes finish.
            if (settingsWindow is { } settings && !await settings.CloseAfterSavingAsync())
            {
                CancelExit(Loc.Error("save_failed"));
                return;
            }
            if (Cards is not null && !await Cards.FlushAsync())
            {
                CancelExit(Loc.Error("card_save_failed"));
                return;
            }
            Bar.CommitScale();
            if (!await Settings.SaveNowAsync())
            {
                // Keep the unsaved settings in memory and let the user retry instead of
                // silently throwing away edits during exit.
                CancelExit(Loc.Error("save_failed"));
                return;
            }
            var shutdown = await Engine.TryCloseAsync();
            if (!shutdown.Ok())
            {
                CancelExit(Loc.Error(shutdown["error"].Str("card_save_failed")));
                return;
            }
            settingsWindow?.CloseHost();
            Cards?.CloseAll();
            captions?.Close();
            overlayMenu?.Dispose();
            Bar.Close();
            updateTimer?.Stop();
            settingsRelease?.Stop();
            tray?.Dispose();
            Exit();
        }
        catch (Exception error)
        {
            LogUi(error);
            if (Engine is not null)
            {
                CancelExit(Loc.T("error.ui"));
            }
        }
    }

    private void SetExitInteraction(bool enabled)
    {
        if (Bar.Content is UIElement bar) bar.IsHitTestVisible = enabled;
        if (captions?.Content is UIElement caption) caption.IsHitTestVisible = enabled;
    }

    private void CancelExit(string message)
    {
        exiting = false;
        SetExitInteraction(true);
        Cards?.ResumeEditing();
        Bar.ShowMessage(message, true);
        _ = RefreshAfterCancelledExit(exitVersion);
    }

    /// <summary>Events are suppressed while exiting. Refresh both snapshots after a failed
    /// exit, without overwriting newer events or touching windows during another exit.</summary>
    private async Task RefreshAfterCancelledExit(int version)
    {
        try
        {
            var stateAtRequest = stateVersion;
            var status = await Engine.CallAsync("status");
            if (exiting || version != exitVersion || Bar?.Content is null) return;
            if (status.Ok() && stateAtRequest == stateVersion)
            {
                OnEngineEvent("state", status);
            }

            var cardsAtRequest = cardsVersion;
            var cards = await Engine.CallAsync("cards.list");
            if (exiting || version != exitVersion || Bar?.Content is null || Cards is null) return;
            if (cards.Ok() && cardsAtRequest == cardsVersion)
            {
                OnEngineEvent("cards.changed", cards);
            }
        }
        catch (Exception error)
        {
            LogUi(error);
        }
    }

    private void LogUi(Exception error)
    {
        try
        {
            var dir = string.IsNullOrEmpty(LogFile) ? Path.GetTempPath() : Path.GetDirectoryName(LogFile)!;
            File.AppendAllText(Path.Combine(dir, "ui-errors.log"), $"{DateTime.Now:O} {error}\n\n");
        }
        catch (IOException)
        {
        }
    }
}
