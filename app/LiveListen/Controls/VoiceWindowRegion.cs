using System.Runtime.InteropServices;
using LiveListen.Interop;
using LiveListen.Services;
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
    private readonly int[] captionBounds = new int[4];
    private int? previousHash;

    public void Apply(bool ribbon, Rect halo, Point[][] lines, double x, double y, double scale, Rect caption, double dpi)
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
                    Set(polygons[layer], i, x + p.X * scale, y + (p.Y - VoiceSizing.RibbonHitHalfWidth) * scale);
                    Set(polygons[layer], (VoiceVisual.Segments + 1) * 2 - 1 - i, x + p.X * scale, y + (p.Y + VoiceSizing.RibbonHitHalfWidth) * scale);
                }
                foreach (var p in polygons[layer]) { Add(p.X); Add(p.Y); }
            }
        }
        // Keep one physical pixel for XAML's antialiased edge.
        SetRect(ellipse, new Rect(x + halo.X * scale - 1, y + halo.Y * scale - 1,
            halo.Width * scale + 2, halo.Height * scale + 2), 1);
        foreach (var value in ellipse) Add(value);
        SetRect(captionBounds, caption, dpi);
        foreach (var value in captionBounds) Add(value);
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
            if (captionBounds[2] > captionBounds[0] && captionBounds[3] > captionBounds[1] &&
                !Union(region, CreateRectRgn(captionBounds[0], captionBounds[1], captionBounds[2], captionBounds[3]))) return;
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
        if (rect.Width <= 0 || rect.Height <= 0)
        {
            Array.Clear(values);
            return;
        }
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
