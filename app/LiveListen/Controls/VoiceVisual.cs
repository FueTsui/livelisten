using System.Diagnostics;
using LiveListen.Services;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.Foundation;
using Windows.UI;
using ShapePath = Microsoft.UI.Xaml.Shapes.Path;

namespace LiveListen.Controls;

/// <summary>
/// Native port of the selected Canvas prototype: five orb contours, four interfering
/// ribbon waves, and cached radial brushes. No full-window blur or extra audio capture.
/// Hidden/unloaded/reduced-motion surfaces have no timer.
/// </summary>
internal sealed class VoiceVisual : Canvas
{
    internal const int Segments = 32;
    internal const double OrbCenter = 116;
    internal const double OrbUnit = 34.0 / 59;
    internal const double HaloRadius = 2.4;
    /// <summary>Radius of the loudest frame; a fixed bound for the window region.</summary>
    internal const double MaxRadius = (59 + 5 + 18) * OrbUnit;
    private const double RibbonReferenceWidth = 664;
    private readonly VoiceMotion motion = new();
    private readonly DispatcherQueueTimer timer;
    private readonly Stopwatch clock = Stopwatch.StartNew();
    private readonly PathFigure[] figures = new PathFigure[6];
    private readonly BezierSegment[][] curves = new BezierSegment[6][];
    private readonly ShapePath[] outlines = new ShapePath[6];
    private readonly Point[] points = new Point[Segments + 3];
    private readonly Ellipse halo = new();
    private readonly Ellipse core = new();
    private readonly VoiceLiquid liquid = new();
    private readonly Canvas contours = new() { Width = 232, Height = 232 };
    private readonly RotateTransform rotation = new() { CenterX = OrbCenter, CenterY = OrbCenter };
    private readonly RadialGradientBrush orbHalo = Radial((0, "#35A6BAFF"), (0.07, "#35A6BAFF"), (0.49, "#207D8DFF"), (1, "#006CBCFF"));
    private readonly RadialGradientBrush ribbonHalo = Radial((0, "#229FBBFF"), (1, "#008BAFFF"));
    private readonly RadialGradientBrush coreBrush = Radial((0, "#525D89C7"), (0.55, "#603F62AD"), (0.85, "#3F7064B5"), (1, "#067D65BA"));
    private LinearGradientBrush stroke = new();
    private readonly ScaleTransform haloScale = new();
    private readonly ScaleTransform coreScale = new();
    private readonly ScaleTransform liquidScale = new();
    private bool ribbon;
    private bool configured;
    private bool darkTheme;
    private bool loaded;
    private double previous;

    public event Action? ShapeChanged;
    public bool IsRibbon => ribbon;
    public double FrameRadius { get; private set; } = 35;
    public Point[][] HitLines { get; } = Enumerable.Range(0, 4).Select(_ => new Point[Segments + 1]).ToArray();

    public VoiceVisual()
    {
        Width = 232;
        Height = 232;
        IsHitTestVisible = false;
        Children.Add(halo);
        Children.Add(contours);
        contours.RenderTransform = rotation;
        PrepareEllipse(halo, haloScale, OrbCenter, OrbCenter, 34 * HaloRadius * 2, 34 * HaloRadius * 2);
        PrepareEllipse(core, coreScale, OrbCenter, OrbCenter, 34 * 0.93 * 2, 34 * 0.93 * 2);
        PrepareEllipse(liquid, liquidScale, OrbCenter, OrbCenter, VoiceLiquid.Diameter, VoiceLiquid.Diameter);
        // A quiet coloured volume supports the flowing liquid instead of a white
        // focal highlight. Its outer gradient circle still has radius R.
        coreBrush.RadiusX = coreBrush.RadiusY = 1 / (2 * 0.93);
        core.Fill = coreBrush;
        for (var layer = 0; layer < figures.Length; layer++)
        {
            var figure = figures[layer] = new PathFigure { IsClosed = true, IsFilled = false };
            var geometry = new PathGeometry();
            geometry.Figures.Add(figure);
            curves[layer] = new BezierSegment[Segments];
            for (var i = 0; i < Segments; i++)
            {
                var segment = curves[layer][i] = new BezierSegment();
                figure.Segments.Add(segment);
            }
            var path = outlines[layer] = new ShapePath { Data = geometry };
            // A faint wide copy supplies local line bloom without a live blur filter.
            if (layer == 5) contours.Children.Insert(0, path);
            else contours.Children.Add(path);
        }
        // Same painter order as the prototype: glow, outlines, then radial core.
        contours.Children.Add(core);
        contours.Children.Add(liquid);
        timer = DispatcherQueue.CreateTimer();
        timer.Tick += (_, _) =>
        {
            var now = clock.Elapsed.TotalSeconds;
            motion.Step(now - previous, now);
            previous = now;
            Draw();
        };
        Loaded += (_, _) => { loaded = true; SyncTimer(); };
        Unloaded += (_, _) => { loaded = false; timer.Stop(); };
        Configure("orb", false, true);
    }

    public void Configure(string style, bool dark, bool animations)
    {
        var nextRibbon = style == "ribbon";
        if (configured && ribbon == nextRibbon && darkTheme == dark && motion.Animations == animations) return;
        configured = true;
        darkTheme = dark;
        ribbon = nextRibbon;
        Height = contours.Height = ribbon ? 48 : 232;
        motion.Animations = animations;
        core.Visibility = liquid.Visibility = ribbon ? Visibility.Collapsed : Visibility.Visible;
        PrepareEllipse(halo, haloScale, 116, ribbon ? 24 : OrbCenter,
            ribbon ? 232 : 34 * HaloRadius * 2, ribbon ? 48 : 34 * HaloRadius * 2);
        halo.Fill = ribbon ? ribbonHalo : orbHalo;
        foreach (var figure in figures) figure.IsClosed = !ribbon;
        var cyan = dark ? "#FFA2E9FA" : "#FF308CB5";
        var violet = dark ? "#FFB7B2FF" : "#FF7060C3";
        var pink = dark ? "#FFE4B7F2" : "#FFAE63A8";
        stroke = ribbon
            ? Gradient((0, "#002689B3"), (0.2, cyan), (0.55, violet), (0.8, pink), (1, "#00B468AD"))
            : Gradient((0, cyan), (0.48, violet), (1, pink));
        // XAML brushes use the shape bounds, not the parent Canvas coordinates.
        // A horizontal ribbon gradient also keeps both ends fully transparent.
        stroke.StartPoint = new Point(0, 0);
        stroke.EndPoint = ribbon ? new Point(1, 0) : new Point(1, 1);
        for (var layer = 0; layer < outlines.Length; layer++)
        {
            var path = outlines[layer];
            path.Stroke = stroke;
            path.Visibility = ribbon && layer == 4 ? Visibility.Collapsed : Visibility.Visible;
            path.StrokeThickness = layer == 5 ? (ribbon ? 5 : 7)
                : ribbon ? (layer == 3 ? 1.5 : 1)
                : (layer == 4 ? 1.6 : 3) * OrbUnit;
            path.Opacity = layer == 5 ? 0.07 : ribbon ? 0.35 + layer * 0.18 : 0.15 + layer * 0.13;
        }
        SyncTimer();
    }

    public void SetPhase(string phase)
    {
        if (motion.Phase == phase) return;
        motion.SetPhase(phase);
        SyncTimer();
    }

    public void SetShown(bool shown)
    {
        if (motion.Visible == shown) return;
        motion.SetVisible(shown);
        SyncTimer();
    }

    public void OnLevel(double value) => motion.OnLevel(value, clock.Elapsed.TotalSeconds);

    private void SyncTimer()
    {
        if (loaded && motion.Running)
        {
            timer.Interval = TimeSpan.FromMilliseconds(motion.IntervalMs);
            if (!timer.IsRunning)
            {
                previous = clock.Elapsed.TotalSeconds;
                timer.Start();
            }
        }
        else timer.Stop();
        // State/theme changes still render once when motion is disabled.
        Draw();
    }

    private void Draw()
    {
        var t = motion.Animations ? motion.Time : 0;
        var level = motion.Animations ? motion.Level : 0;
        var breath = Math.Sin(t * 1.4) * 0.5 + 0.5;
        var radius = (59 + breath * 5 + level * 18) * OrbUnit;
        FrameRadius = radius;
        rotation.Angle = ribbon ? 0 : t * (motion.Processing ? 0.30 : 0.07) * 180 / Math.PI;
        if (ribbon)
        {
            haloScale.ScaleX = haloScale.ScaleY = 1;
        }
        else
        {
            // Full 2.4 R halo fits the canvas even at the loudest frame. Transforms
            // preserve text size and avoid measuring the window on animation ticks.
            haloScale.ScaleX = haloScale.ScaleY = radius / 34;
            coreScale.ScaleX = coreScale.ScaleY = radius / 34;
            liquidScale.ScaleX = liquidScale.ScaleY = radius / 34;
            liquid.Draw(t, level, motion.Phase == "listening");
            var inner = 10 * OrbUnit / (radius * HaloRadius);
            orbHalo.GradientStops[1].Offset = inner;
            orbHalo.GradientStops[2].Offset = inner + 0.45 * (1 - inner);
        }
        for (var layer = 0; layer < figures.Length; layer++)
        {
            if (ribbon && layer == 4) continue;
            var contour = layer == 5 ? (ribbon ? 3 : 4) : layer;
            // Reuse a bounded cubic path; preserve the reference's wave phases,
            // frequencies and layer count instead of replacing them with one sine.
            for (var i = -1; i <= Segments + 1; i++)
            {
                var u = (double)i / Segments;
                if (ribbon)
                {
                    var sourceX = 25 + u * (RibbonReferenceWidth - 50);
                    var unit = 232 / RibbonReferenceWidth;
                    var envelope = Math.Pow(Math.Max(0, Math.Sin(Math.Clamp(u, 0, 1) * Math.PI)), 1.8);
                    var wave = Math.Sin(sourceX * 0.032 - t * 2 + contour * 0.8) * Math.Sin(sourceX * 0.014 + t * 0.6);
                    points[i + 1] = new Point(sourceX * unit, 24 + wave * (2 + breath * 2 + level * 24) * envelope * unit);
                }
                else
                {
                    var angle = u * Math.Tau;
                    var r = radius + OrbUnit * ((4 + level * 12) * Math.Sin(angle * 3 + t * 1.5 + contour * 0.55)
                        + 3 * Math.Cos(angle * 5 - t));
                    points[i + 1] = new Point(OrbCenter + Math.Cos(angle) * r, OrbCenter + Math.Sin(angle) * r);
                }
            }
            figures[layer].StartPoint = points[1];
            if (layer < HitLines.Length) Array.Copy(points, 1, HitLines[layer], 0, Segments + 1);
            for (var i = 0; i < Segments; i++)
            {
                var a = points[i];
                var b = points[i + 1];
                var c = points[i + 2];
                var d = points[i + 3];
                var segment = curves[layer][i];
                segment.Point1 = new Point(b.X + (c.X - a.X) / 6, b.Y + (c.Y - a.Y) / 6);
                segment.Point2 = new Point(c.X - (d.X - b.X) / 6, c.Y - (d.Y - b.Y) / 6);
                segment.Point3 = c;
            }
        }
        ShapeChanged?.Invoke();
    }

    private static void PrepareEllipse(FrameworkElement element, ScaleTransform scale, double x, double y, double width, double height)
    {
        element.Width = width;
        element.Height = height;
        element.RenderTransformOrigin = new Point(0.5, 0.5);
        element.RenderTransform = scale;
        SetLeft(element, x - width / 2);
        SetTop(element, y - height / 2);
    }

    private static RadialGradientBrush Radial(params (double Offset, string Color)[] stops)
    {
        var brush = new RadialGradientBrush
        {
            Center = new Point(0.5, 0.5), GradientOrigin = new Point(0.5, 0.5), RadiusX = 0.5, RadiusY = 0.5,
        };
        foreach (var (offset, color) in stops) brush.GradientStops.Add(Stop(offset, color));
        return brush;
    }

    private static GradientStop Stop(double offset, string color)
    {
        var value = Convert.ToUInt32(color[1..], 16);
        return new GradientStop
        {
            Offset = offset,
            Color = Color.FromArgb((byte)(value >> 24), (byte)(value >> 16), (byte)(value >> 8), (byte)value),
        };
    }

    private static LinearGradientBrush Gradient(params (double Offset, string Color)[] stops)
    {
        var brush = new LinearGradientBrush { StartPoint = new Point(0, 0), EndPoint = new Point(1, 1) };
        foreach (var (offset, color) in stops) brush.GradientStops.Add(Stop(offset, color));
        return brush;
    }
}
