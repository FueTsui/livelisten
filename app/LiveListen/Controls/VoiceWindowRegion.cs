using System.Runtime.InteropServices;
using LiveListen.Interop;
using Windows.Foundation;

namespace LiveListen.Controls;

/// <summary>
/// Restricts the HWND to the visual's painted neighborhood and visible caption areas.
/// Unlike HTTRANSPARENT, a window region also lets another process receive input in
/// excluded space. Reuses the existing render tick; no mouse hook or polling timer.
/// </summary>
internal sealed class VoiceWindowRegion(nint hwnd)
{
    private readonly POINT[][] polygons = Enumerable.Range(0, 4).Select(_ => new POINT[(VoiceVisual.Segments + 1) * 2]).ToArray();
    private readonly int[] ellipse = new int[4];
    private readonly int[][] captions = [new int[4], new int[4]];
    private int? previousHash;

    public void Apply(bool ribbon, double radius, Point[][] lines, double x, double y, double scale, Rect status, Rect text, double dpi)
    {
        var hash = ribbon ? 1 : 0;
        void Add(int value) => hash = unchecked(hash * 31 + value);
        if (ribbon)
        {
            for (var layer = 0; layer < polygons.Length; layer++)
            {
                for (var i = 0; i <= VoiceVisual.Segments; i++)
                {
                    var p = lines[layer][i];
                    // Includes local line bloom and cubic interpolation between samples.
                    Set(polygons[layer], i, x + p.X * scale, y + (p.Y - 6) * scale);
                    Set(polygons[layer], (VoiceVisual.Segments + 1) * 2 - 1 - i, x + p.X * scale, y + (p.Y + 6) * scale);
                }
                foreach (var p in polygons[layer]) { Add(p.X); Add(p.Y); }
            }
            // GDI's integer ellipse is slightly inset from XAML's antialiased
            // ellipse. Keep one physical pixel of room for the feathered edge.
            SetRect(ellipse, new Rect(x - 1, y - 1, 232 * scale + 2, 48 * scale + 2), 1);
        }
        else
        {
            var extent = radius * VoiceVisual.HaloRadius + 1;
            SetRect(ellipse, new Rect(x + (VoiceVisual.OrbCenter - extent) * scale, y + (VoiceVisual.OrbCenter - extent) * scale, 2 * extent * scale, 2 * extent * scale), 1);
        }
        foreach (var value in ellipse) Add(value);
        SetRect(captions[0], status, dpi);
        SetRect(captions[1], text, dpi);
        foreach (var rect in captions) foreach (var value in rect) Add(value);
        if (previousHash == hash) return;

        var region = CreateRectRgn(0, 0, 0, 0);
        if (region == 0) return;
        try
        {
            if (ribbon)
            {
                foreach (var polygon in polygons)
                    if (!Union(region, CreatePolygonRgn(polygon, polygon.Length, 1))) return;
            }
            if (!Union(region, CreateEllipticRgn(ellipse[0], ellipse[1], ellipse[2], ellipse[3]))) return;
            foreach (var rect in captions)
                if (rect[2] > rect[0] && rect[3] > rect[1] && !Union(region, CreateRectRgn(rect[0], rect[1], rect[2], rect[3]))) return;
            if (SetWindowRgn(hwnd, region, false) != 0)
            {
                region = 0; // Ownership transfers to Windows only on success.
                previousHash = hash;
            }
        }
        finally { if (region != 0) DeleteObject(region); }
    }

    private static void Set(POINT[] points, int i, double x, double y) =>
        points[i] = new POINT { X = (int)Math.Round(x), Y = (int)Math.Round(y) };

    private static void SetRect(int[] values, Rect rect, double scale)
    {
        values[0] = (int)Math.Floor(rect.X * scale);
        values[1] = (int)Math.Floor(rect.Y * scale);
        values[2] = (int)Math.Ceiling(rect.Right * scale);
        values[3] = (int)Math.Ceiling(rect.Bottom * scale);
    }

    private static bool Union(nint destination, nint source)
    {
        if (source == 0) return false;
        try { return CombineRgn(destination, destination, source, 2) != 0; }
        finally { DeleteObject(source); }
    }

    [DllImport("gdi32.dll")] private static extern nint CreateRectRgn(int left, int top, int right, int bottom);
    [DllImport("gdi32.dll")] private static extern nint CreateEllipticRgn(int left, int top, int right, int bottom);
    [DllImport("gdi32.dll")] private static extern nint CreatePolygonRgn([In] POINT[] points, int count, int mode);
    [DllImport("gdi32.dll")] private static extern int CombineRgn(nint dest, nint first, nint second, int mode);
    [DllImport("gdi32.dll")] [return: MarshalAs(UnmanagedType.Bool)] private static extern bool DeleteObject(nint obj);
    [DllImport("user32.dll")] private static extern int SetWindowRgn(nint window, nint region, [MarshalAs(UnmanagedType.Bool)] bool redraw);
}
