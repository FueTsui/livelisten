using System.Text.Json.Nodes;
using LiveListen.Controls;
using LiveListen.Interop;
using LiveListen.Services;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Animation;
using Windows.Graphics;
using Windows.Foundation;
using Windows.UI;

namespace LiveListen.Views;

/// <summary>
/// A transparent desktop overlay: only the voice shape and text are painted. The visual
/// and captions share one no-activate window and move together. Controls live in its
/// context menu, the tray and the settings window, never in a permanent card or toolbar.
/// </summary>
internal sealed partial class BarWindow : Window
{
    private const double PanelWidth = 312;
    private readonly App app;
    private readonly SettingsStore settings;
    private readonly DispatcherQueue dispatcher;
    private readonly Grid root = new() { Background = new SolidColorBrush(Palette.Transparent) };
    private readonly StackPanel body = new() { Width = PanelWidth, Margin = new Thickness(8, 8, 8, 40), VerticalAlignment = VerticalAlignment.Top };
    private readonly VoiceVisual voice = new();
    private readonly Viewbox voiceBox = new() { Stretch = Stretch.Uniform, HorizontalAlignment = HorizontalAlignment.Center };
    private readonly VoiceWindowRegion hitRegion;
    private readonly Grid main = new() { Background = new SolidColorBrush(Palette.Transparent) };
    private readonly TextBlock status = Caption(16, 24);
    private readonly TextBlock statusShadow = Caption(16, 24);
    private readonly TextBlock live = Caption(16, 24);
    private readonly TextBlock liveShadow = Caption(16, 24);
    private readonly SolidColorBrush textBrush = new();
    private readonly SolidColorBrush errorBrush = new(Color.FromArgb(255, 255, 153, 164));
    private readonly Grid statusHost = new();
    private readonly ScrollViewer liveScroll = new()
    {
        MaxHeight = 48,
        VerticalScrollBarVisibility = ScrollBarVisibility.Hidden,
        VerticalScrollMode = ScrollMode.Enabled,
        HorizontalScrollMode = ScrollMode.Disabled,
        Margin = new Thickness(0, 4, 0, 0),
        Visibility = Visibility.Collapsed,
    };
    private readonly DispatcherQueueTimer messageTimer;
    private readonly DispatcherQueueTimer lingerTimer;
    private Storyboard? zoomAnimation;
    private double zoomStart;
    private double zoomCaptionStart;
    private double zoomCaptionEnd;
    private double textGap = 8;
    private readonly Grid captionPanel = new() { Width = PanelWidth, HorizontalAlignment = HorizontalAlignment.Center };
    private readonly StackPanel captionBody = new();
    private readonly ScrollViewer captionViewport = new()
    {
        VerticalScrollBarVisibility = ScrollBarVisibility.Hidden,
        VerticalScrollMode = ScrollMode.Enabled,
        HorizontalScrollMode = ScrollMode.Disabled,
    };
    // Storyboard may clone a property-path target. Always write the transform that
    // is currently attached, including after Stop/retarget and caption-side changes.
    private TranslateTransform captionMotion => (TranslateTransform)captionPanel.RenderTransform;
    private readonly Surface captionSurface;
    private ScaleTransform zoomTransform => (ScaleTransform)voiceBox.RenderTransform;
    private string phase = "idle";
    private string? noteStatus;
    private string heldReason = "";
    private string text = "";
    private string unwritten = "";
    private readonly UnwrittenClipboard clipboard;
    private string? message;
    private bool messageIsError;
    private bool autoShown;
    private bool placed;
    private bool fitQueued;
    private bool refreshing;
    private double voiceScale = 1.5;
    private double targetScale = 1.5;
    private double layoutScale = 3;
    private double captionReserve = 240;
    private Point? zoomAnchor;
    private bool screenActive = true;
    private Task visibilityCalls = Task.CompletedTask;

    public BarWindow(App app, Func<string, bool>? copyText = null)
    {
        this.app = app;
        settings = app.Settings;
        dispatcher = DispatcherQueue;
        clipboard = new UnwrittenClipboard(copyText ?? (value => Win32.CopyText(Overlay.Handle(this), value)));
        clipboard.Changed += UpdateView;
        Title = Loc.T("app.name");
        voice.HorizontalAlignment = HorizontalAlignment.Center;
        voiceBox.Child = voice;
        voiceBox.RenderTransformOrigin = new Point(0.5, 0.5);
        voiceBox.RenderTransform = new ScaleTransform();
        main.Children.Add(voiceBox);
        main.PointerWheelChanged += (_, e) =>
        {
            var point = e.GetCurrentPoint(main);
            if (point.Properties.IsHorizontalMouseWheel) return;
            e.Handled = true;
            RequestScale(VoiceSizing.Wheel(targetScale, point.Properties.MouseWheelDelta, voice.IsRibbon));
        };
        // The transparent drag surface captures no audio and never takes keyboard focus.
        _ = new WindowDrag(this, main, SavePosition, keepOnTop: true);
        AutomationProperties.SetName(main, Loc.T("voice.drag"));
        status.MaxLines = statusShadow.MaxLines = 2;
        status.MinHeight = statusShadow.MinHeight = 18;
        status.TextTrimming = statusShadow.TextTrimming = TextTrimming.CharacterEllipsis;
        AddText(statusHost, statusShadow, status);
        var lines = new Grid();
        AddText(lines, liveShadow, live);
        liveScroll.Content = lines;
        body.Children.Add(main);
        var captionBackdrop = new Grid();
        captionSurface = new Surface(captionBackdrop);
        captionPanel.Children.Add(captionBackdrop);
        captionBody.Children.Add(statusHost);
        captionBody.Children.Add(liveScroll);
        captionViewport.Content = captionBody;
        captionPanel.Children.Add(captionViewport);
        captionPanel.RenderTransform = new TranslateTransform();
        body.Children.Add(captionPanel);
        root.Children.Add(body);
        Content = root;
        Overlay.Prepare(this);
        hitRegion = new VoiceWindowRegion(Overlay.Handle(this));
        voice.ShapeChanged += OnVoiceFrame;
        root.ContextRequested += (_, e) => { e.Handled = true; ShowContextMenu(); };
        AppWindow.Closing += (_, e) =>
        {
            if (!app.Exiting) { e.Cancel = true; Pin(false); }
        };
        AppWindow.Changed += (_, e) =>
        {
            if (e.DidVisibilityChange) SyncVoice();
            if (e.DidPositionChange && placed) QueueFit();
        };
        messageTimer = Timer(() => { message = null; UpdateView(); HideIfDone(); });
        lingerTimer = Timer(() =>
        {
            if (phase == "idle" && unwritten.Length == 0) { text = ""; UpdateView(); HideIfDone(); }
        });
        Closed += (_, _) =>
        {
            clipboard.Changed -= UpdateView;
            clipboard.Clear();
            messageTimer.Stop();
            lingerTimer.Stop();
            voice.SetShown(false);
            zoomAnimation?.Stop(); zoomAnimation = null;
        };
        var scale = Overlay.Scale(this);
        AppWindow.Resize(new SizeInt32((int)((PanelWidth + 16) * scale), (int)(156 * scale)));
        Refresh();
        root.Loaded += (_, _) => QueueFit();
    }

    public bool Pinned { get; private set; }
    public RectInt32? ScreenRect => placed
        ? new RectInt32(AppWindow.Position.X, AppWindow.Position.Y, AppWindow.Size.Width, AppWindow.Size.Height)
        : null;
    private bool IsShown => Overlay.IsVisible(this);

    private static TextBlock Caption(double size, double lineHeight) => new()
    {
        FontSize = size,
        LineHeight = lineHeight,
        TextWrapping = TextWrapping.Wrap,
        TextAlignment = TextAlignment.Center,
        HorizontalAlignment = HorizontalAlignment.Stretch,
    };

    private static void AddText(Grid host, TextBlock shadow, TextBlock foreground)
    {
        shadow.IsHitTestVisible = false;
        AutomationProperties.SetAccessibilityView(shadow, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
        // A small text-only offset keeps captions legible without a rectangular background.
        shadow.RenderTransform = new TranslateTransform { X = 1, Y = 1 };
        host.Children.Add(shadow);
        host.Children.Add(foreground);
    }

    private DispatcherQueueTimer Timer(Action tick)
    {
        var timer = dispatcher.CreateTimer();
        timer.IsRepeating = false;
        timer.Tick += (_, _) => tick();
        return timer;
    }

    public void Pin(bool on)
    {
        Pinned = on;
        // The next start, also with Windows, shows the bar the way it was left.
        settings.Set("general.barPinned", on);
        if (on) { autoShown = false; ShowNow(); }
        else HideNow();
    }

    private void ShowNow()
    {
        EnsurePlaced();
        // A display may have gone away since the bar was last shown.
        EnsureOnScreen();
        Overlay.Show(this);
        SyncVoice();
        NotifyVisibility(true);
        QueueFit();
    }

    private void HideNow()
    {
        FinishScale();
        autoShown = false;
        voice.SetShown(false);
        Overlay.Hide(this);
        NotifyVisibility(false);
    }

    /// <summary>No animation frames while nothing can be seen: hidden, locked, or display off.</summary>
    private void SyncVoice() => voice.SetShown(screenActive && IsShown);

    public void SetScreenActive(bool active)
    {
        screenActive = active;
        SyncVoice();
    }

    private void OnVoiceFrame()
    {
        // Only the ribbon's outline changes the window region from frame to frame; the
        // orb's region is fixed and is set when the layout or the scale changes.
        if (voice.IsRibbon) UpdateHitRegion();
    }

    private void EnsurePlaced()
    {
        if (!placed) { Fit(); Place(); placed = true; }
    }

    /// <summary>Tells the engine in call order: a quick show after a hide must end as shown.</summary>
    private void NotifyVisibility(bool on)
    {
        var previous = visibilityCalls;
        visibilityCalls = Send();
        async Task Send()
        {
            try
            {
                await previous;
                await app.Engine.CallAsync("ui.visibility", new JsonObject { ["bar"] = on });
            }
            catch (Exception)
            {
                // Level events are cosmetic; the next change sends the state again.
            }
        }
    }

    /// <summary>Where the visual's center is now, also before the content has loaded.</summary>
    private Point CurrentCenter()
    {
        if (ScreenCenter() is { } center) return center;
        var dpi = Overlay.Scale(this);
        return new Point(AppWindow.Position.X + (body.Width / 2 + 8) * dpi,
            AppWindow.Position.Y + (main.Height / 2 + body.Margin.Top) * dpi);
    }

    /// <summary>Keeps the whole drawn shape on the display nearest to the wanted center.</summary>
    private Point ClampCenter(Point center)
    {
        var x = double.IsFinite(center.X) ? Math.Clamp(center.X, -1e9, 1e9) : 0;
        var y = double.IsFinite(center.Y) ? Math.Clamp(center.Y, -1e9, 1e9) : 0;
        var bounds = DisplayArea.GetFromPoint(new PointInt32((int)Math.Round(x), (int)Math.Round(y)),
            DisplayAreaFallback.Nearest).OuterBounds;
        var dpi = Overlay.Scale(this);
        var marginX = Math.Min(bounds.Width / 2.0, ((voice.IsRibbon ? 116 : CaptionExtent) * targetScale + 8) * dpi);
        var marginY = Math.Min(bounds.Height / 2.0, (CaptionExtent * targetScale + 8) * dpi);
        return new Point(Math.Clamp(x, bounds.X + marginX, bounds.X + bounds.Width - marginX),
            Math.Clamp(y, bounds.Y + marginY, bounds.Y + bounds.Height - marginY));
    }

    private void MoveCenter(Point center)
    {
        zoomAnchor = center;
        Fit();
    }

    private Point DefaultCenter()
    {
        var work = DisplayArea.Primary.WorkArea;
        return new Point(work.X + work.Width / 2.0,
            work.Y + work.Height - (voice.Height * voiceScale / 2 + 80) * Overlay.Scale(this));
    }

    private void SaveCenter(Point center) => settings.Set("general.barVisualCenter", new JsonObject
    {
        ["x"] = (long)Math.Round(center.X), ["y"] = (long)Math.Round(center.Y),
    });

    /// <summary>Pulls the bar back when its display was removed or its saved place is off screen.</summary>
    public void EnsureOnScreen()
    {
        if (!placed) return;
        var center = CurrentCenter();
        var visible = ClampCenter(center);
        if (Math.Abs(visible.X - center.X) < 1 && Math.Abs(visible.Y - center.Y) < 1) return;
        MoveCenter(visible);
        SaveCenter(visible);
    }

    /// <summary>Tray command: back to the bottom center of the primary display.</summary>
    public void ResetPosition()
    {
        EnsurePlaced();
        var center = ClampCenter(DefaultCenter());
        MoveCenter(center);
        SaveCenter(center);
        settings.Set("general.barPosition", new JsonObject { ["x"] = AppWindow.Position.X, ["y"] = AppWindow.Position.Y });
        Pin(true);
    }

    private void AutoShow()
    {
        if (!IsShown) { autoShown = true; ShowNow(); }
    }

    private void HideIfDone()
    {
        if (autoShown && !Pinned && phase == "idle" && unwritten.Length == 0 && message is null && text.Length == 0)
            HideNow();
    }

    private void Place()
    {
        var dpi = Overlay.Scale(this);
        Point center;
        if (settings.Get("general.barVisualCenter") is JsonObject saved)
            center = new Point(saved["x"].Num(), saved["y"].Num());
        else if (settings.Get("general.barPosition") is JsonObject pos)
            // Migrate the former tightly fitted window origin without moving its visual.
            center = new Point(pos["x"].Num() + (Math.Max(PanelWidth, voice.Width * voiceScale) / 2 + 8) * dpi,
                pos["y"].Num() + (voice.Height * voiceScale / 2 + 8) * dpi);
        else center = DefaultCenter();
        // A saved place on a display that is gone would leave the visual off screen.
        center = ClampCenter(center);
        AppWindow.Move(new PointInt32((int)Math.Round(center.X - (body.Width / 2 + 8) * dpi),
            (int)Math.Round(center.Y - (main.Height / 2 + body.Margin.Top) * dpi)));
        // Persist the migrated center even when the user never drags or zooms.
        // A later style switch must not reinterpret the old window origin at restart.
        SaveCenter(center);
    }

    private void SavePosition(PointInt32 p)
    {
        var center = ScreenCenter();
        if (zoomAnimation is not null) zoomAnchor = center;
        settings.Set("general.barPosition", new JsonObject { ["x"] = p.X, ["y"] = p.Y });
        if (center is { } value)
            settings.Set("general.barVisualCenter", new JsonObject { ["x"] = (long)Math.Round(value.X), ["y"] = (long)Math.Round(value.Y) });
    }

    private Point? ScreenCenter()
    {
        if (!root.IsLoaded) return null;
        var center = voice.TransformToVisual(root).TransformPoint(new Point(voice.Width / 2, voice.Height / 2));
        var dpi = Overlay.Scale(this);
        return new Point(AppWindow.Position.X + center.X * dpi, AppWindow.Position.Y + center.Y * dpi);
    }

    private void SetLayoutScale()
    {
        // Keep HWND dimensions and the visual's center fixed throughout every zoom.
        // Only independent render transforms are animated by the compositor.
        layoutScale = VoiceSizing.Maximum;
        voiceBox.Width = voice.Width * layoutScale;
        voiceBox.Height = main.Height = voice.Height * layoutScale;
        body.Width = Math.Max(PanelWidth, voiceBox.Width);
        root.Width = body.Width + 16;
        SetCaptionReserve();
        ApplyScale(voiceScale);
    }

    private void SetCaptionReserve()
    {
        // Reserve a stable window size even while idle so text changes and zoom
        // keep the voice visual anchored.
        body.Margin = new Thickness(8, captionReserve + 8, 8, 0);
        root.Height = main.Height + captionReserve * 2 + 16;
    }

    private double CurrentScale => zoomAnimation is null ? voiceScale
        : VoiceSizing.Advance(zoomStart, targetScale, zoomAnimation.GetCurrentTime().TotalSeconds);

    // Measure spacing from the animated contour envelope, not its transparent halo
    // canvas. These fixed envelopes include the loudest frame without audio jitter.
    private double CaptionExtent => voice.IsRibbon ? 14 : 62;

    private double CaptionOffset(double scale) => CaptionExtent * scale - main.Height / 2;

    private void PlaceCaption()
    {
        var center = ScreenCenter();
        if (center is { } point)
        {
            var dpi = Overlay.Scale(this);
            // The visual, rather than the large transparent HWND, selects its monitor.
            var display = DisplayArea.GetFromPoint(new PointInt32((int)Math.Round(point.X), (int)Math.Round(point.Y)),
                DisplayAreaFallback.Nearest);
            // All text attached to the voice visual can use the taskbar strip.
            var bounds = display.OuterBounds;
            ConstrainCaption(bounds, dpi);
            var left = (body.Width + 16 - captionPanel.ActualWidth) / 2;
            var minLeft = (bounds.X - AppWindow.Position.X) / dpi + 8;
            var maxLeft = (bounds.X + bounds.Width - AppWindow.Position.X) / dpi - captionPanel.ActualWidth - 8;
            captionMotion.X = Math.Clamp(left, minLeft, Math.Max(minLeft, maxLeft)) - left;
        }
        if (zoomAnimation is null) ApplyScale(voiceScale);
        else if (Math.Abs(zoomCaptionStart - CaptionOffset(zoomStart)) > .01 || Math.Abs(zoomCaptionEnd - CaptionOffset(targetScale)) > .01)
            SetScaleTarget(targetScale, true);
    }

    private void ConstrainCaption(RectInt32 bounds, double dpi)
    {
        var width = Math.Min(PanelWidth, Math.Max(1, bounds.Width / dpi - 16));
        var height = Math.Max(1, bounds.Height / dpi - 16);
        if (captionPanel.Width == width && captionViewport.MaxHeight == height) return;
        captionPanel.Width = width;
        // Keep every line scrollable even with large type on a small display.
        captionViewport.MaxHeight = height;
        root.UpdateLayout();
    }

    private void ApplyScale(double scale)
    {
        zoomTransform.ScaleX = zoomTransform.ScaleY = scale / layoutScale;
        captionMotion.Y = CaptionOffset(scale);
    }

    private void RequestScale(double value) => SetScaleTarget(value, false);

    private void SetScaleTarget(double value, bool reposition)
    {
        value = VoiceSizing.Clamp(value, voice.IsRibbon);
        if (value == targetScale && !reposition) return;
        voiceScale = CurrentScale;
        zoomAnimation?.Stop();
        zoomAnimation = null;
        ApplyScale(voiceScale);
        targetScale = value;
        if (!Theme.AnimationsEnabled || !IsShown) { FinishScale(); return; }
        zoomStart = voiceScale;
        var animation = new Storyboard();
        var duration = new Duration(TimeSpan.FromSeconds(Math.Abs(targetScale - zoomStart) / VoiceSizing.Speed));
        void Animate(DependencyObject target, string property, double from, double to)
        {
            var track = new DoubleAnimation { From = from, To = to, Duration = duration };
            Storyboard.SetTarget(track, target);
            Storyboard.SetTargetProperty(track, property);
            animation.Children.Add(track);
        }
        Animate(voiceBox, "(UIElement.RenderTransform).(ScaleTransform.ScaleX)", zoomStart / layoutScale, targetScale / layoutScale);
        Animate(voiceBox, "(UIElement.RenderTransform).(ScaleTransform.ScaleY)", zoomStart / layoutScale, targetScale / layoutScale);
        zoomCaptionStart = CaptionOffset(zoomStart);
        zoomCaptionEnd = CaptionOffset(targetScale);
        Animate(captionPanel, "(UIElement.RenderTransform).(TranslateTransform.Y)", zoomCaptionStart, zoomCaptionEnd);
        animation.Completed += (_, _) => { if (ReferenceEquals(zoomAnimation, animation)) FinishScale(); };
        zoomAnimation = animation;
        UpdateHitRegion();
        animation.Begin();
    }

    private void FinishScale()
    {
        if (zoomAnimation is null && voiceScale == targetScale) return;
        zoomAnimation?.Stop();
        zoomAnimation = null;
        voiceScale = targetScale;
        ApplyScale(voiceScale);
        UpdateHitRegion();
        settings.Set(voice.IsRibbon ? "appearance.ribbonScale" : "appearance.voiceScale", voiceScale);
        SavePosition(AppWindow.Position);
    }

    internal void CommitScale() => FinishScale();

    private void QueueFit()
    {
        if (fitQueued) return;
        fitQueued = true;
        // Keep caption layout/scrolling with incoming text events. Low priority can
        // leave the newest lines below the viewport during a continuous input burst.
        dispatcher.TryEnqueue(DispatcherQueuePriority.Normal, () =>
        {
            fitQueued = false;
            Fit();
            if (liveScroll.Visibility == Visibility.Visible)
            {
                liveScroll.UpdateLayout();
                liveScroll.ChangeView(null, liveScroll.ScrollableHeight, null, true);
            }
        });
    }

    private void Fit()
    {
        var anchor = zoomAnchor ?? ScreenCenter();
        var dpi = Overlay.Scale(this);
        root.Measure(new Size(body.Width + 16, double.PositiveInfinity));
        var width = (int)Math.Ceiling((body.Width + 16) * dpi);
        var height = Math.Max(1, (int)Math.Ceiling(root.DesiredSize.Height * dpi));
        var position = AppWindow.Position;
        // Always anchor the graphic itself as the text changes.
        var x = anchor is { } a ? (int)Math.Round(a.X - (body.Width / 2 + 8) * dpi) : position.X;
        var y = anchor is { } b ? (int)Math.Round(b.Y - (main.Height / 2 + body.Margin.Top) * dpi) : position.Y;
        if (position.X != x || position.Y != y || AppWindow.Size.Width != width || AppWindow.Size.Height != height)
            AppWindow.MoveAndResize(new RectInt32(x, y, width, height));
        zoomAnchor = null;
        root.UpdateLayout();
        PlaceCaption();
        UpdateHitRegion();
    }

    private void UpdateHitRegion()
    {
        if (!root.IsLoaded || !IsShown || voice.ActualWidth == 0) return;
        var dpi = Overlay.Scale(this);
        var center = main.TransformToVisual(root).TransformPoint(new Point(main.ActualWidth / 2, main.ActualHeight / 2));
        var scale = zoomAnimation is null ? voiceScale : Math.Max(zoomStart, targetScale);
        var caption = new Rect();
        if (captionPanel.Visibility == Visibility.Visible)
        {
            // Cover both ends of the independent animation, including the panel's shadow.
            var from = CaptionOffset(zoomAnimation is null ? voiceScale : zoomStart);
            var to = CaptionOffset(targetScale);
            var first = Math.Min(from, to);
            var last = Math.Max(from, to);
            caption = new Rect((body.Width + 16 - captionPanel.ActualWidth) / 2 + captionMotion.X - 32,
                body.Margin.Top + main.Height + textGap + first - 32, captionPanel.ActualWidth + 64,
                captionPanel.ActualHeight + last - first + 72);
        }
        // The orb uses its loudest radius, so breathing does not rebuild the region each frame.
        hitRegion.Apply(voice.IsRibbon, VoiceVisual.MaxRadius, voice.HitLines,
            (center.X - voice.Width * scale / 2) * dpi, (center.Y - voice.Height * scale / 2) * dpi,
            scale * dpi, caption, new Rect(), dpi);
    }

    private void ShowContextMenu()
    {
        app.ShowOverlayMenu(this, root, [
            new(Loc.T(phase == "idle" ? "voice.menu.dictate" : "bar.mic.stop"), app.ToggleDictation),
            TrayItem.Separator,
            new(Loc.T("tray.settings"), () => app.OpenSettings("appearance")),
            new(Loc.T("voice.menu.hide"), () => { Pin(false); app.BarClosedByUser(); }),
            new(Loc.T("tray.exit"), app.Quit),
        ]);
    }

    public void OnState(JsonObject e)
    {
        var d = e["dictation"] as JsonObject;
        var next = d?["phase"].Str("idle") ?? "idle";
        if (phase == "idle" && next != "idle")
        {
            text = "";
            unwritten = "";
            clipboard.Clear();
            heldReason = "";
            message = null;
            lingerTimer.Stop();
            AutoShow();
        }
        phase = next;
        noteStatus = d is not null && d["purpose"].Str() == "note"
            ? (d["trigger"].Str() == "hold" ? "status.note.hold" : "status.note")
            : null;
        if (d is not null && next != "idle")
        {
            heldReason = d["paused"].Bool() ? "paused" : d["blocked"].Str();
        }
        UpdateView();
    }

    public void OnLevel(double value) => voice.OnLevel(value);

    public void OnText(string value, string notWritten)
    {
        text = value;
        unwritten = notWritten;
        _ = clipboard.UpdateAsync(notWritten);
        UpdateView();
    }

    public void OnDone(JsonObject e)
    {
        var reason = e["reason"].Str();
        // A spoken note becomes a card; nothing is left to copy or type.
        var keep = reason != "cancelled" && e["purpose"].Str() != "note";
        text = keep ? e["text"].Str() : "";
        unwritten = keep ? e["unwritten"].Str() : "";
        _ = clipboard.UpdateAsync(unwritten, retry: true);
        if (reason == "timeout")
        {
            ShowMessage(Loc.Error("timeout"), true);
        }
        if (unwritten.Length == 0)
        {
            lingerTimer.Interval = TimeSpan.FromMilliseconds(1500);
            lingerTimer.Start();
        }
        UpdateView();
    }

    public void OnNotice(string code)
    {
        // These states use the actual clipboard outcome instead of a temporary notice
        // obscuring it or claiming success before a clipboard owner releases its lock.
        if (code is "paused" or "write_failed" or "elevated" or "no_target")
        {
            heldReason = code == "write_failed" ? "paused" : code;
            message = null;
            messageTimer.Stop();
            UpdateView();
            return;
        }
        var error = code is "password" or "write_failed" or "elevated" or "no_target" or "no_voice";
        ShowMessage(Loc.T("notice." + code), error);
    }

    public void ShowMessage(string value, bool error)
    {
        message = value;
        messageIsError = error;
        messageTimer.Stop();
        messageTimer.Interval = TimeSpan.FromSeconds(error ? 6 : 3.5);
        messageTimer.Start();
        AutoShow();
        UpdateView();
    }

    public void Refresh()
    {
        if (refreshing) return;
        refreshing = true;
        try
        {
            var center = ScreenCenter();
            var look = Theme.Look(settings, "bar");
            var fontSize = Math.Clamp(settings.Num("dictation.fontSize", 16), 12, 64);
            var wasRibbon = voice.IsRibbon;
            if (zoomAnimation is not null && wasRibbon != (settings.Str("appearance.voiceStyle", "orb") == "ribbon"))
            {
                // Preserve the previous style's requested size even if it changes mid-zoom.
                zoomAnimation?.Stop(); zoomAnimation = null;
                settings.Set(wasRibbon ? "appearance.ribbonScale" : "appearance.voiceScale", targetScale);
            }
            var dark = Theme.IsDark(settings.Str("appearance.theme", "system"));
            root.RequestedTheme = dark ? ElementTheme.Dark : ElementTheme.Light;
            voice.Configure(settings.Str("appearance.voiceStyle", "orb"), dark, Theme.AnimationsEnabled);
            captionReserve = Math.Max(0, fontSize * 6 + look.Padding * 2 + 4 + 64 + 40
                - (voice.Height / 2 - CaptionExtent) * VoiceSizing.Maximum);
            zoomAnchor = center;
            if (zoomAnimation is null || wasRibbon != voice.IsRibbon)
            {
                zoomAnimation?.Stop(); zoomAnimation = null;
                voiceScale = targetScale = VoiceSizing.Clamp(settings.Num(voice.IsRibbon ? "appearance.ribbonScale" : "appearance.voiceScale",
                    voice.IsRibbon ? VoiceSizing.RibbonDefault : 1.5), voice.IsRibbon);
                SetLayoutScale();
            }
            else if (!Theme.AnimationsEnabled) FinishScale();
            SetCaptionReserve();
            statusHost.MaxWidth = liveScroll.MaxWidth = PanelWidth;
            statusHost.HorizontalAlignment = liveScroll.HorizontalAlignment = HorizontalAlignment.Center;
            textGap = Math.Clamp(settings.Num("appearance.voiceTextGap", 8), 0, 64);
            captionPanel.Margin = new Thickness(0, textGap, 0, 0);
            captionSurface.Apply(look);
            liveShadow.Opacity = statusShadow.Opacity = look.Shadow == "none" ? 0 : 1;
            captionBody.Padding = new Thickness(look.Padding);
            var color = look.Text;
            textBrush.Color = color;
            live.Foreground = textBrush;
            foreach (var label in new[] { live, liveShadow, status, statusShadow })
            {
                label.FontSize = fontSize;
                label.LineHeight = fontSize * 1.5;
            }
            status.MinHeight = statusShadow.MinHeight = fontSize * 1.5;
            liveScroll.MaxHeight = fontSize * 3;
            var lightText = 0.299 * color.R + 0.587 * color.G + 0.114 * color.B >= 128;
            var shadow = new SolidColorBrush(lightText ? Color.FromArgb(220, 0, 0, 0) : Color.FromArgb(220, 255, 255, 255));
            liveShadow.Foreground = statusShadow.Foreground = shadow;
            UpdateView();
            if (zoomAnimation is null) { Fit(); zoomAnchor = null; }
            else QueueFit();
        }
        finally { refreshing = false; }
    }

    private void UpdateView()
    {
        voice.SetPhase(phase);
        var held = unwritten.Length > 0 || (phase != "idle" && heldReason.Length > 0);
        var showLive = !held && text.Length > 0 && settings.Bool("dictation.showText");
        var visibleText = showLive ? text : "";
        var state = message ?? (held ? ClipboardStatus() : phase switch
        {
            "connecting" => Loc.T("status.connecting"),
            "finishing" => Loc.T("status.finishing"),
            "listening" when !showLive => Loc.T(noteStatus ?? "voice.listening"),
            _ => unwritten.Length > 0 ? Loc.T("bar.unwritten") : "",
        });
        var inlineFinishing = phase == "finishing" && showLive && message is null;
        visibleText += inlineFinishing ? "  " + state : "";
        if (inlineFinishing) state = "";
        var liveVisibility = showLive ? Visibility.Visible : Visibility.Collapsed;
        var statusVisibility = state.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
        var panelVisibility = showLive || state.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
        var changed = live.Text != visibleText || status.Text != state
            || liveScroll.Visibility != liveVisibility || statusHost.Visibility != statusVisibility
            || captionPanel.Visibility != panelVisibility;
        if (live.Text != visibleText) live.Text = liveShadow.Text = visibleText;
        if (status.Text != state)
        {
            status.Text = statusShadow.Text = state;
            ToolTipService.SetToolTip(status, state);
        }
        var foreground = message is not null && messageIsError ? errorBrush : textBrush;
        if (!ReferenceEquals(status.Foreground, foreground)) status.Foreground = foreground;
        if (liveScroll.Visibility != liveVisibility) liveScroll.Visibility = liveVisibility;
        if (statusHost.Visibility != statusVisibility) statusHost.Visibility = statusVisibility;
        if (captionPanel.Visibility != panelVisibility) captionPanel.Visibility = panelVisibility;
        if (changed) QueueFit();
    }

    private string ClipboardStatus()
    {
        if (phase != "idle" && heldReason is "paused" or "no_target" or "elevated")
        {
            var suffix = !clipboard.HasText ? ".empty" : clipboard.Copied ? "" : clipboard.Failed ? ".copy_failed" : ".pending";
            return Loc.T("status." + heldReason + suffix);
        }
        return Loc.T(!clipboard.HasText ? "bar.unwritten" : clipboard.Copied ? "status.clipboard_copied"
            : clipboard.Failed ? "status.clipboard_failed" : "status.clipboard_pending");
    }
}
