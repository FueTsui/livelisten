using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;

namespace LiveListen.Controls;

/// <summary>List symbols drawn as geometry, independent of optional icon-font glyphs.</summary>
internal static class NoteListIcons
{
    public static PathIcon Create(string kind, Brush? foreground = null)
    {
        var geometry = new GeometryGroup { FillRule = FillRule.Nonzero };
        for (var row = 0; row < 3; row++)
        {
            var y = 3.5 + row * 6.5;
            Stroke(geometry, [new Point(8, y), new Point(22, y)], 1.4);
            switch (kind)
            {
                case "dash":
                    Stroke(geometry, [new Point(1, y), new Point(4.5, y)], 1.3);
                    break;
                case "number":
                    Digit(geometry, row + 1, y - 2.4);
                    break;
                default:
                    geometry.Children.Add(new EllipseGeometry { Center = new Point(3, y), RadiusX = 1.25, RadiusY = 1.25 });
                    break;
            }
        }
        var icon = new PathIcon { Data = geometry, Width = 22, Height = 22 };
        if (foreground is not null) icon.Foreground = foreground;
        return icon;
    }

    private static void Digit(GeometryGroup geometry, int digit, double top)
    {
        Point[] points = digit switch
        {
            1 => [new(1.5, top + 1), new(3, top), new(3, top + 4.6)],
            2 => [new(1.25, top + 0.8), new(2.1, top), new(3.5, top), new(4.15, top + 0.8),
                new(3.8, top + 1.8), new(1.25, top + 4.6), new(4.3, top + 4.6)],
            _ => [new(1.3, top + 0.25), new(3.4, top), new(4.1, top + 0.8), new(3.35, top + 2.2),
                new(2.4, top + 2.2), new(3.5, top + 2.2), new(4.2, top + 3.4), new(3.5, top + 4.5), new(1.3, top + 4.5)],
        };
        Stroke(geometry, points, 0.85);
    }

    private static void Stroke(GeometryGroup geometry, Point[] points, double width)
    {
        var radius = width / 2;
        foreach (var point in points)
            geometry.Children.Add(new EllipseGeometry { Center = point, RadiusX = radius, RadiusY = radius });
        for (var i = 1; i < points.Length; i++)
        {
            var a = points[i - 1];
            var b = points[i];
            var length = Math.Sqrt(Math.Pow(b.X - a.X, 2) + Math.Pow(b.Y - a.Y, 2));
            if (length == 0) continue;
            var dx = (b.Y - a.Y) / length * radius;
            var dy = (a.X - b.X) / length * radius;
            var polygon = new PathFigure { StartPoint = new Point(a.X + dx, a.Y + dy), IsClosed = true };
            polygon.Segments.Add(new LineSegment { Point = new Point(b.X + dx, b.Y + dy) });
            polygon.Segments.Add(new LineSegment { Point = new Point(b.X - dx, b.Y - dy) });
            polygon.Segments.Add(new LineSegment { Point = new Point(a.X - dx, a.Y - dy) });
            var path = new PathGeometry();
            path.Figures.Add(polygon);
            geometry.Children.Add(path);
        }
    }
}
