using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.Foundation;
using Windows.UI;

namespace LiveListen.Controls;

/// <summary>
/// A bounded liquid volume inside the orb: three soft colour masses and eighteen
/// suspended particles. Reuses native shapes/brushes; only transforms and opacity
/// change on the parent's existing tick. No blur filter, clipping surface or timer.
/// </summary>
internal sealed class VoiceLiquid : Canvas
{
    internal const double Diameter = 34 * 0.93 * 2;
    private const double Radius = Diameter / 2;
    private readonly Ellipse[] masses = new Ellipse[3];
    private readonly CompositeTransform[] massTransforms = new CompositeTransform[3];
    private readonly Ellipse[] particles = new Ellipse[18];
    private readonly CompositeTransform[] particleTransforms = new CompositeTransform[18];

    public VoiceLiquid()
    {
        Width = Height = Diameter;
        IsHitTestVisible = false;
        (double Width, double Height, Color Colour)[] sizes =
        [
            (42, 29, Color.FromArgb(190, 65, 179, 207)),
            (34, 43, Color.FromArgb(180, 102, 91, 199)),
            (37, 26, Color.FromArgb(170, 173, 104, 194)),
        ];
        for (var i = 0; i < masses.Length; i++)
        {
            var (width, height, colour) = sizes[i];
            var mass = masses[i] = new Ellipse
            {
                Width = width, Height = height, Fill = FluidBrush(colour),
                RenderTransformOrigin = new Point(0.5, 0.5),
                RenderTransform = massTransforms[i] = new CompositeTransform(),
            };
            SetLeft(mass, Radius - width / 2);
            SetTop(mass, Radius - height / 2);
            Children.Add(mass);
        }
        Brush[] colours =
        [
            new SolidColorBrush(Color.FromArgb(255, 92, 200, 222)),
            new SolidColorBrush(Color.FromArgb(255, 140, 128, 220)),
            new SolidColorBrush(Color.FromArgb(255, 188, 132, 211)),
        ];
        for (var i = 0; i < particles.Length; i++)
        {
            var size = 0.7 + i % 4 * 0.2;
            var particle = particles[i] = new Ellipse
            {
                Width = size * 1.25, Height = size, Fill = colours[i % colours.Length],
                RenderTransformOrigin = new Point(0.5, 0.5),
                RenderTransform = particleTransforms[i] = new CompositeTransform(),
            };
            SetLeft(particle, Radius - particle.Width / 2);
            SetTop(particle, Radius - particle.Height / 2);
            Children.Add(particle);
        }
    }

    public void Draw(double time, double level, bool listening)
    {
        var activity = listening ? Math.Clamp(level * 1.5, 0, 1) : 0;
        for (var i = 0; i < masses.Length; i++)
        {
            var phase = time * 0.7 + i * Math.Tau / 3;
            var transform = massTransforms[i];
            transform.ScaleX = 0.88 + Math.Sin(phase * 0.9) * 0.12 + activity * 0.08;
            transform.ScaleY = 0.86 + Math.Cos(phase * 1.1) * 0.12 + activity * 0.08;
            transform.Rotation = phase * 180 / Math.PI + i * 37;
            // The complete ellipse, including its transparent feather, stays
            // inside the core without requiring a per-frame mask or layout.
            var extent = Math.Max(masses[i].Width * transform.ScaleX, masses[i].Height * transform.ScaleY) / 2;
            var travel = Math.Max(0, Radius - extent - 1.5) * 0.8;
            transform.TranslateX = Math.Cos(phase * 0.67 + i) * travel;
            transform.TranslateY = Math.Sin(phase * 0.67 + i) * travel;
            masses[i].Opacity = (0.42 + activity * 0.5) * (0.85 + Math.Sin(phase + i) * 0.15);
        }
        for (var i = 0; i < particles.Length; i++)
        {
            var seed = i * 2.399963229728653;
            var angle = seed + time * (0.38 + i % 3 * 0.07);
            var distance = Radius * (0.18 + (i % 9) / 8.0 * 0.58
                + Math.Sin(time * 0.8 + seed) * 0.035 * activity);
            var transform = particleTransforms[i];
            transform.TranslateX = Math.Cos(angle) * distance;
            transform.TranslateY = Math.Sin(angle + Math.Sin(time * 0.45 + seed) * 0.2) * distance * 0.86;
            transform.Rotation = angle * 180 / Math.PI + 90;
            transform.ScaleX = transform.ScaleY = 0.8 + activity * 0.45;
            particles[i].Opacity = (0.05 + activity * 0.72) * (0.55 + 0.45 * Math.Sin(angle * 1.7 + seed) * Math.Sin(angle * 1.7 + seed));
        }
    }

    private static RadialGradientBrush FluidBrush(Color colour)
    {
        var brush = new RadialGradientBrush
        {
            Center = new Point(0.5, 0.5), GradientOrigin = new Point(0.5, 0.5), RadiusX = 0.5, RadiusY = 0.5,
        };
        foreach (var (offset, alpha) in new[] { (0.0, 1.0), (0.38, 0.8), (0.72, 0.3), (1.0, 0.0) })
            brush.GradientStops.Add(new GradientStop { Offset = offset, Color = Color.FromArgb((byte)(colour.A * alpha), colour.R, colour.G, colour.B) });
        return brush;
    }
}
