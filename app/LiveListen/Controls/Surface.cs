using System.Numerics;
using LiveListen.Services;
using Microsoft.UI.Composition;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Hosting;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;

namespace LiveListen.Controls;

/// <summary>
/// A rounded panel with an optional outline and a drop shadow, drawn behind XAML content.
/// The panel is a XAML rectangle; the shadow uses its alpha mask so it follows the corners.
/// </summary>
internal sealed class Surface
{
    private readonly Rectangle shape = new();
    private readonly SpriteVisual sprite;
    private readonly DropShadow shadow;

    /// <param name="host">An empty grid sized like the panel.</param>
    public Surface(Grid host)
    {
        var shadowHost = new Grid();
        host.Children.Add(shadowHost);
        host.Children.Add(shape);
        var compositor = ElementCompositionPreview.GetElementVisual(host).Compositor;
        sprite = compositor.CreateSpriteVisual();
        shadow = compositor.CreateDropShadow();
        shadow.Color = Windows.UI.Color.FromArgb(255, 0, 0, 0);
        shadow.Mask = shape.GetAlphaMask();
        sprite.Shadow = shadow;
        ElementCompositionPreview.SetElementChildVisual(shadowHost, sprite);
        shape.SizeChanged += (_, e) => sprite.Size = new Vector2((float)e.NewSize.Width, (float)e.NewSize.Height);
    }

    public void Apply(SurfaceLook look)
    {
        shape.Fill = new SolidColorBrush(look.Background);
        shape.Stroke = look.Border.A > 0 ? new SolidColorBrush(look.Border) : null;
        shape.StrokeThickness = look.Border.A > 0 ? 1 : 0;
        shape.RadiusX = look.Radius;
        shape.RadiusY = look.Radius;
        switch (look.Shadow)
        {
            case "strong":
                shadow.BlurRadius = 28;
                shadow.Opacity = look.Dark ? 0.55f : 0.3f;
                shadow.Offset = new Vector3(0, 8, 0);
                sprite.IsVisible = true;
                break;
            case "soft":
                shadow.BlurRadius = 16;
                shadow.Opacity = look.Dark ? 0.4f : 0.18f;
                shadow.Offset = new Vector3(0, 4, 0);
                sprite.IsVisible = true;
                break;
            default:
                sprite.IsVisible = false;
                break;
        }
    }
}
