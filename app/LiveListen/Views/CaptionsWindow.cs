using System.Text.Json.Nodes;
using LiveListen.Controls;
using LiveListen.Services;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;

namespace LiveListen.Views;

/// <summary>
/// Live captions: a transparent strip near the bottom of the screen showing the latest lines.
/// </summary>
internal sealed partial class CaptionsWindow : Window
{
    private readonly App app;
    private readonly SettingsStore settings;
    private readonly DispatcherQueue dispatcher;
    private readonly Surface surface;
    private readonly WindowDrag drag;

    private readonly Grid root = new() { Background = new SolidColorBrush(Palette.Transparent) };
    private readonly Grid frame = new() { VerticalAlignment = VerticalAlignment.Top };
    private readonly Grid surfaceHost = new();
    private readonly StackPanel linesPanel = new() { Spacing = 6 };
    private readonly GlyphButton close = new("", Loc.T("captions.close"), size: 24, iconSize: 9);
    private readonly DispatcherQueueTimer errorTimer;

    private SurfaceLook look;
    private JsonArray? lines;
    private readonly TextBlock?[] blocks = new TextBlock[5];
    private readonly SolidColorBrush textBrush = new();
    private readonly SolidColorBrush secondaryBrush = new();
    private double fontSize = 24;
    private int lineCount = 2;
    private bool styled;
    private string? error;
    private bool placed;
    private bool linesSeen;
    private bool fitQueued;
    private bool clickThrough;

    public CaptionsWindow(App app)
    {
        this.app = app;
        settings = app.Settings;
        dispatcher = DispatcherQueue;
        Title = Loc.T("tray.captions");

        close.View.HorizontalAlignment = HorizontalAlignment.Right;
        close.View.VerticalAlignment = VerticalAlignment.Top;
        close.View.Margin = new Thickness(0, 4, 4, 0);
        close.View.Opacity = 0;
        close.Click += () => _ = app.Engine.CallAsync("captions.set", new JsonObject { ["on"] = false });
        frame.Children.Add(surfaceHost);
        frame.Children.Add(linesPanel);
        frame.Children.Add(close.View);
        frame.PointerEntered += (_, _) => close.View.Opacity = 1;
        frame.PointerExited += (_, _) => close.View.Opacity = 0;
        root.Children.Add(frame);
        Content = root;
        surface = new Surface(surfaceHost);
        drag = new WindowDrag(this, frame, SavePosition);
        Overlay.Prepare(this);
        AppWindow.Closing += (sender, e) =>
        {
            if (!app.Exiting)
            {
                e.Cancel = true;
                _ = app.Engine.CallAsync("captions.set", new JsonObject { ["on"] = false });
            }
        };
        errorTimer = dispatcher.CreateTimer();
        errorTimer.IsRepeating = false;
        errorTimer.Interval = TimeSpan.FromSeconds(6);
        errorTimer.Tick += (_, _) =>
        {
            error = null;
            Render();
        };
        Refresh();
        _ = ShowRecentLines();
        root.Loaded += (_, _) => QueueFit();
    }

    /// <summary>Shows lines recognized before this window existed, without blocking on the engine.</summary>
    private async Task ShowRecentLines()
    {
        var reply = await app.Engine.CallAsync("captions.lines");
        // A live update that arrived meanwhile is newer than this snapshot.
        if (!app.Exiting && !linesSeen && reply["lines"] is JsonArray recent)
        {
            OnLines(recent);
        }
    }

    public void ShowOverlay()
    {
        if (!placed)
        {
            Fit();
            Place();
            placed = true;
        }
        if (!Overlay.IsVisible(this))
        {
            Overlay.Show(this);
            Overlay.SetClickThrough(this, clickThrough);
        }
        QueueFit();
    }

    public void HideOverlay()
    {
        if (Overlay.IsVisible(this))
        {
            Overlay.Hide(this);
        }
    }

    public void OnState(JsonObject e) => Render();

    public void OnLines(JsonArray? value)
    {
        linesSeen = true;
        lines = value;
        Render();
    }

    public void OnError(string code)
    {
        error = Loc.Error(code);
        errorTimer.Stop();
        errorTimer.Start();
        Render();
    }

    /// <summary>Re-reads style, size and click-through from settings.</summary>
    public void Refresh()
    {
        var nextLook = Theme.Look(settings, "captions");
        var nextSize = Math.Clamp(settings.Num("captions.fontSize", 24), 12, 64);
        var nextCount = (int)Math.Clamp(settings.Num("captions.lines", 2), 1, 4);
        var textChanged = !styled || nextLook != look || nextSize != fontSize || nextCount != lineCount;
        if (!styled || nextLook != look)
        {
            look = nextLook;
            root.RequestedTheme = look.Dark ? ElementTheme.Dark : ElementTheme.Light;
            frame.Margin = new Thickness(look.ShadowMargin);
            surface.Apply(look);
            linesPanel.Padding = new Thickness(Math.Max(8, look.Padding + 4), look.Padding, Math.Max(8, look.Padding + 4), look.Padding);
            close.Apply(look);
            textBrush.Color = look.Text;
            secondaryBrush.Color = look.Secondary;
        }
        fontSize = nextSize;
        lineCount = nextCount;
        styled = true;
        var through = settings.Bool("captions.clickThrough");
        if (through != clickThrough)
        {
            clickThrough = through;
            drag.Enabled = !through;
            if (Overlay.IsVisible(this))
            {
                Overlay.SetClickThrough(this, through);
            }
        }
        if (textChanged) Render();
        // A settings slider changes the live window in the same callback. Saving is
        // debounced separately; pointer input must not starve a low-priority resize.
        Fit();
    }

    private void Render()
    {
        Span<int> shown = stackalloc int[4];
        var count = 0;
        for (var i = (lines?.Count ?? 0) - 1; i >= 0 && count < lineCount; i--)
            if (lines![i] is JsonObject line && line["text"].Str().Length > 0) shown[count++] = i;
        var used = 0;
        if (count == 0) SetBlock(used++, error ?? Loc.T("captions.waiting"), fontSize * 0.75, secondaryBrush);
        for (var i = count - 1; i >= 0; i--)
            SetBlock(used++, lines![shown[i]]!["text"].Str(), fontSize, textBrush);
        if (count > 0 && error is not null) SetBlock(used++, error, fontSize * 0.6, secondaryBrush);
        while (linesPanel.Children.Count > used) linesPanel.Children.RemoveAt(linesPanel.Children.Count - 1);
        QueueFit();
    }

    private void SetBlock(int index, string value, double size, Brush brush)
    {
        var block = blocks[index] ??= new TextBlock { TextWrapping = TextWrapping.Wrap };
        if (block.Text != value) block.Text = value;
        // XAML stores these values at float precision; compare at that precision
        // so an unchanged fractional line height does not trigger a native setter.
        if ((float)block.FontSize != (float)size) block.FontSize = size;
        if ((float)block.LineHeight != (float)(size * 1.35)) block.LineHeight = size * 1.35;
        if (!ReferenceEquals(block.Foreground, brush)) block.Foreground = brush;
        if (linesPanel.Children.Count <= index) linesPanel.Children.Add(block);
    }

    private double WidthDip()
    {
        var work = Overlay.WorkArea(this);
        var percent = Math.Clamp(settings.Num("captions.widthPercent", 60), 30, 100) / 100;
        return work.Width * percent / Overlay.Scale(this);
    }

    private void QueueFit()
    {
        if (fitQueued)
        {
            return;
        }
        fitQueued = true;
        dispatcher.TryEnqueue(DispatcherQueuePriority.Normal, () =>
        {
            fitQueued = false;
            Fit();
        });
    }

    private void Fit()
    {
        var width = WidthDip();
        frame.Width = width - 2 * look.ShadowMargin;
        Overlay.Fit(this, root, width);
    }

    private void Place()
    {
        if (settings.Get("general.captionsPosition") is JsonObject pos)
        {
            Overlay.PlaceAt(this, (int)pos["x"].Num(), (int)pos["y"].Num());
        }
        else
        {
            // Bottom center of the primary screen, kept clear of the dictation bar.
            var work = Microsoft.UI.Windowing.DisplayArea.Primary.WorkArea;
            var scale = Overlay.Scale(this);
            var size = AppWindow.Size;
            var bottom = work.Y + work.Height - (int)(40 * scale);
            if (app.Bar.ScreenRect is { } bar && bar.Y < bottom && bar.Y + bar.Height > bottom - size.Height
                && bar.X < work.X + (work.Width + size.Width) / 2 && bar.X + bar.Width > work.X + (work.Width - size.Width) / 2)
            {
                bottom = bar.Y + (int)(8 * scale);
            }
            AppWindow.Move(new PointInt32(work.X + (work.Width - size.Width) / 2, bottom - size.Height));
        }
    }

    private void SavePosition(PointInt32 p) =>
        settings.Set("general.captionsPosition", new JsonObject { ["x"] = p.X, ["y"] = p.Y });
}
