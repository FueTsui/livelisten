using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace LiveListen.Controls;

/// <summary>A native toggle with a note-sized glyph and a full header-height hit target.</summary>
internal sealed partial class CardCheckBox : CheckBox
{
    public CardCheckBox()
    {
        MinWidth = 0;
        MinHeight = 28;
        Padding = new Thickness(0);
        VerticalAlignment = VerticalAlignment.Center;
    }

    protected override void OnApplyTemplate()
    {
        base.OnApplyTemplate();
        // WinUI resolves CheckBoxSize as a StaticResource in its shared template.
        // Local resource overrides cannot resize it; adjust only this control's visual.
        if (GetTemplateChild("NormalRectangle") is FrameworkElement box)
        {
            box.Width = box.Height = 14;
            if (VisualTreeHelper.GetParent(box) is FrameworkElement container)
                container.Width = container.Height = 14;
        }
        if (GetTemplateChild("CheckGlyph") is AnimatedIcon glyph)
        {
            glyph.Width = glyph.Height = 14;
            glyph.Margin = new Thickness(0);
            if (glyph.FallbackIconSource is FontIconSource fallback) fallback.FontSize = 9;
        }
    }
}
