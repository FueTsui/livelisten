using LiveListen.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace LiveListen.Controls;

/// <summary>
/// A light icon button for overlay surfaces, whose colors come from the surface style
/// rather than the app theme. Works with the mouse in windows that never take focus.
/// </summary>
internal sealed class GlyphButton
{
    private readonly FontIcon icon;
    private readonly TextBlock? label;
    private Color hover;
    private Color pressed;
    private bool over;
    private bool down;

    public GlyphButton(string glyph, string name, string? text = null, double size = 32, double iconSize = 14)
    {
        icon = new FontIcon { Glyph = glyph, FontSize = iconSize };
        View = new GlyphGrid(this)
        {
            MinWidth = size,
            Height = size,
            CornerRadius = new CornerRadius(4),
            Background = new SolidColorBrush(Palette.Transparent),
        };
        if (text is null)
        {
            View.Children.Add(icon);
        }
        else
        {
            label = new TextBlock { Text = text, FontSize = 12, VerticalAlignment = VerticalAlignment.Center };
            var row = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                Spacing = 6,
                Padding = new Thickness(8, 0, 10, 0),
                VerticalAlignment = VerticalAlignment.Center,
            };
            row.Children.Add(icon);
            row.Children.Add(label);
            View.Children.Add(row);
        }
        AutomationProperties.SetName(View, name);
        AutomationProperties.SetAutomationControlType(View, AutomationControlType.Button);
        View.PointerEntered += (_, _) => { over = true; Paint(); };
        View.PointerExited += (_, _) => { over = false; down = false; Paint(); };
        View.PointerPressed += (_, e) => { down = true; Paint(); e.Handled = true; };
        View.PointerReleased += (_, _) => { down = false; Paint(); };
        View.PointerCanceled += (_, _) => { down = false; Paint(); };
        View.Tapped += (_, e) =>
        {
            e.Handled = true;
            Click?.Invoke();
        };
    }

    public Grid View { get; }

    public event Action? Click;

    /// <summary>Runs the click, e.g. when a screen reader invokes the button.</summary>
    internal void Invoke() => Click?.Invoke();

    public string Glyph
    {
        get => icon.Glyph;
        set => icon.Glyph = value;
    }

    public string Text
    {
        set
        {
            if (label is not null)
            {
                label.Text = value;
            }
        }
    }

    public bool Checked { get; set; }

    public void Apply(SurfaceLook look)
    {
        var brush = new SolidColorBrush(look.Text);
        icon.Foreground = brush;
        if (label is not null)
        {
            label.Foreground = brush;
        }
        hover = look.Hover;
        pressed = look.Pressed;
        Paint();
    }

    private void Paint()
    {
        var color = down ? pressed : over || Checked ? hover : Palette.Transparent;
        View.Background = new SolidColorBrush(color);
    }

    public void Refresh() => Paint();
}

/// <summary>The button's surface; reports itself as an invokable button to screen readers.</summary>
internal sealed partial class GlyphGrid(GlyphButton button) : Grid
{
    protected override AutomationPeer OnCreateAutomationPeer() => new GlyphPeer(this, button);
}

internal sealed partial class GlyphPeer(GlyphGrid owner, GlyphButton button)
    : FrameworkElementAutomationPeer(owner), Microsoft.UI.Xaml.Automation.Provider.IInvokeProvider
{
    protected override object GetPatternCore(PatternInterface pattern) =>
        pattern == PatternInterface.Invoke ? this : base.GetPatternCore(pattern);

    protected override AutomationControlType GetAutomationControlTypeCore() => AutomationControlType.Button;

    public void Invoke() => button.Invoke();
}

internal static class Palette
{
    public static Color Transparent => Color.FromArgb(0, 255, 255, 255);
}
