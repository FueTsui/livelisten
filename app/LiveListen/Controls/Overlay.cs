using LiveListen.Interop;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Input;
using Windows.Foundation;
using Windows.Graphics;
using static LiveListen.Interop.Win32;

namespace LiveListen.Controls;

/// <summary>
/// Helpers for borderless, transparent, always-on-top windows that never take focus,
/// so typing continues in the app the user is working in.
/// </summary>
internal static unsafe class Overlay
{
    // Paints the window background black: with the frame extended over the client area,
    // black counts as transparent, so only the XAML content is visible.
    [System.Runtime.InteropServices.UnmanagedCallersOnly]
    private static nint EraseBlack(nint hwnd, uint msg, nint wParam, nint lParam, nuint id, nuint data)
    {
        if (msg == WM_ERASEBKGND)
        {
            RECT rect;
            GetClientRect(hwnd, &rect);
            FillRect(wParam, &rect, GetStockObject(BLACK_BRUSH));
            return 1;
        }
        if (msg == WM_NCCALCSIZE && wParam != 0)
        {
            // The whole window is client area, so no frame is drawn even if the
            // presenter restores its frame styles.
            return 0;
        }
        return DefSubclassProc(hwnd, msg, wParam, lParam);
    }

    public static nint Handle(Window window) => WinRT.Interop.WindowNative.GetWindowHandle(window);

    public static double Scale(Window window)
    {
        var dpi = GetDpiForWindow(Handle(window));
        return dpi == 0 ? 1 : dpi / 96.0;
    }

    /// <param name="floating">
    /// True for panels that stay on top and never take focus. False for desktop cards, which
    /// sit with other windows and take focus when clicked so their text can be edited.
    /// </param>
    public static void Prepare(Window window, bool floating = true)
    {
        var presenter = OverlappedPresenter.Create();
        presenter.SetBorderAndTitleBar(false, false);
        presenter.IsResizable = false;
        presenter.IsMaximizable = false;
        presenter.IsMinimizable = false;
        presenter.IsAlwaysOnTop = floating;
        window.AppWindow.SetPresenter(presenter);
        window.AppWindow.IsShownInSwitchers = false;

        var hwnd = Handle(window);
        // The presenter keeps a dialog frame; remove it so no edge is drawn.
        var style = GetWindowLongPtr(hwnd, GWL_STYLE);
        SetWindowLongPtr(hwnd, GWL_STYLE, style & ~(WS_CAPTION | WS_THICKFRAME | WS_SYSMENU));
        var ex = GetWindowLongPtr(hwnd, GWL_EXSTYLE);
        ex = floating
            ? (ex | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST) & ~WS_EX_APPWINDOW
            : (ex | WS_EX_TOOLWINDOW) & ~(WS_EX_APPWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST);
        SetWindowLongPtr(hwnd, GWL_EXSTYLE, ex);
        uint corner = DWMWCP_DONOTROUND;
        DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, ref corner, sizeof(uint));
        uint border = DWMWA_COLOR_NONE;
        DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, ref border, sizeof(uint));
        var margins = new MARGINS { Left = -1, Right = -1, Top = -1, Bottom = -1 };
        DwmExtendFrameIntoClientArea(hwnd, ref margins);
        SetWindowSubclass(hwnd, &EraseBlack, 1, 0);
        InvalidateRect(hwnd, null, true);
        SetWindowPos(hwnd, 0, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        window.SystemBackdrop = new TransparentBackdrop();
    }

    public static bool IsVisible(Window window) => IsWindowVisible(Handle(window));

    public static void Show(Window window)
    {
        var hwnd = Handle(window);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        RaiseFloating(window);
    }

    /// <summary>Restore the front of the topmost band without stealing input focus.</summary>
    public static void RaiseFloating(Window window) =>
        SetWindowPos(Handle(window), HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);

    /// <summary>Move a floating visual and restore its order above other topmost windows atomically.</summary>
    public static void MoveFloating(Window window, PointInt32 position) =>
        SetWindowPos(Handle(window), HWND_TOPMOST, position.X, position.Y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);

    /// <summary>
    /// Shows a normal window in front with keyboard focus, also when another app is in the
    /// foreground. The bar and captions never activate, so a click on them does not give this
    /// app the foreground right that <see cref="Window.Activate"/> alone relies on.
    /// </summary>
    /// <returns>True when the window ended up in the foreground.</returns>
    public static bool BringToFront(Window window)
    {
        var hwnd = Handle(window);
        ShowWindow(hwnd, IsIconic(hwnd) ? SW_RESTORE : SW_SHOW);
        window.Activate();
        var front = GetForegroundWindow();
        if (front == hwnd)
        {
            return true;
        }
        // Sharing input with the foreground thread for a moment lets Windows accept the change.
        var frontThread = front == 0 ? 0 : GetWindowThreadProcessId(front, out _);
        var own = GetCurrentThreadId();
        var attached = frontThread != 0 && frontThread != own && AttachThreadInput(own, frontThread, true);
        try
        {
            BringWindowToTop(hwnd);
            SetForegroundWindow(hwnd);
        }
        finally
        {
            if (attached)
            {
                AttachThreadInput(own, frontThread, false);
            }
        }
        if (GetForegroundWindow() == hwnd)
        {
            return true;
        }
        // Windows kept the focus elsewhere: at least put the window above the others.
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        return false;
    }

    /// <summary>Puts a normal window above the other normal windows without taking focus.</summary>
    public static void RaiseQuiet(Window window) =>
        SetWindowPos(Handle(window), HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);

    /// <summary>Shows a window without moving it above others or taking focus.</summary>
    public static void ShowQuiet(Window window) => ShowWindow(Handle(window), SW_SHOWNOACTIVATE);

    public static void Hide(Window window) => ShowWindow(Handle(window), SW_HIDE);

    public static void SetClickThrough(Window window, bool on)
    {
        var hwnd = Handle(window);
        var ex = GetWindowLongPtr(hwnd, GWL_EXSTYLE);
        ex = on ? ex | WS_EX_LAYERED | WS_EX_TRANSPARENT : ex & ~(WS_EX_LAYERED | WS_EX_TRANSPARENT);
        SetWindowLongPtr(hwnd, GWL_EXSTYLE, ex);
        if (on)
        {
            SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
        }
        SetWindowPos(hwnd, 0, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
    }

    /// <summary>
    /// Resizes the window to the content's desired height at the given width. A window in
    /// the lower half of the screen keeps its bottom edge, unless keepTop preserves the
    /// position of text being selected while a card's bottom toolbar expands.
    /// </summary>
    public static void Fit(Window window, FrameworkElement root, double widthDip, bool keepTop = false)
    {
        root.Measure(new Size(widthDip, double.PositiveInfinity));
        var scale = Scale(window);
        var width = (int)Math.Ceiling(widthDip * scale);
        var height = Math.Max(1, (int)Math.Ceiling(root.DesiredSize.Height * scale));
        var app = window.AppWindow;
        var pos = app.Position;
        var size = app.Size;
        if (size.Width == width && size.Height == height)
        {
            return;
        }
        var work = WorkArea(window);
        var lower = pos.Y + size.Height / 2 > work.Y + work.Height / 2;
        var x = pos.X + (size.Width - width) / 2;
        var y = lower && !keepTop ? pos.Y + size.Height - height : pos.Y;
        app.MoveAndResize(Clamp(new RectInt32(x, y, width, height), work));
    }

    public static RectInt32 WorkArea(Window window) =>
        DisplayArea.GetFromWindowId(window.AppWindow.Id, DisplayAreaFallback.Nearest).WorkArea;

    /// <summary>Places the window centered horizontally near the bottom of the primary screen.</summary>
    public static void PlaceBottomCenter(Window window, double bottomDip)
    {
        var work = DisplayArea.Primary.WorkArea;
        var size = window.AppWindow.Size;
        var gap = (int)(bottomDip * Scale(window));
        window.AppWindow.Move(new PointInt32(work.X + (work.Width - size.Width) / 2, work.Y + work.Height - size.Height - gap));
    }

    /// <summary>Moves to a saved position, kept inside the screen that contains it.</summary>
    public static void PlaceAt(Window window, int x, int y)
    {
        var work = DisplayArea.GetFromPoint(new PointInt32(x, y), DisplayAreaFallback.Nearest).WorkArea;
        var size = window.AppWindow.Size;
        var rect = Clamp(new RectInt32(x, y, size.Width, size.Height), work);
        window.AppWindow.Move(new PointInt32(rect.X, rect.Y));
    }

    private static RectInt32 Clamp(RectInt32 r, RectInt32 work)
    {
        var x = Math.Clamp(r.X, work.X, Math.Max(work.X, work.X + work.Width - r.Width));
        var y = Math.Clamp(r.Y, work.Y, Math.Max(work.Y, work.Y + work.Height - r.Height));
        return new RectInt32(x, y, r.Width, r.Height);
    }
}

/// <summary>Moves a window while the pointer is dragged on a handle element.</summary>
internal sealed class WindowDrag
{
    private readonly Window window;
    private readonly UIElement handle;
    private readonly Action<PointInt32> finished;
    private readonly bool keepOnTop;
    private POINT start;
    private PointInt32 origin;
    private bool dragging;
    private bool moved;

    public WindowDrag(Window window, UIElement handle, Action<PointInt32> finished, bool keepOnTop = false)
    {
        this.window = window;
        this.handle = handle;
        this.finished = finished;
        this.keepOnTop = keepOnTop;
        handle.PointerPressed += OnPressed;
        handle.PointerMoved += OnMoved;
        handle.PointerReleased += OnReleased;
        handle.PointerCaptureLost += OnCaptureLost;
    }

    public bool Enabled { get; set; } = true;

    /// <summary>Adjusts the window position the pointer would give, for snapping.</summary>
    public Func<PointInt32, PointInt32>? Snap { get; set; }

    private void OnPressed(object sender, PointerRoutedEventArgs e)
    {
        var point = e.GetCurrentPoint(null);
        if (!Enabled || !point.Properties.IsLeftButtonPressed)
        {
            return;
        }
        origin = window.AppWindow.Position;
        // The input queue can deliver a short drag after the physical mouse has
        // already reached its end. Keep the press event's position, not the live
        // GetCursorPos value. Overlay windows have no non-client frame, so root
        // DIPs map directly to the window's client origin in screen pixels.
        var scale = Overlay.Scale(window);
        start = new POINT
        {
            X = origin.X + (int)Math.Round(point.Position.X * scale),
            Y = origin.Y + (int)Math.Round(point.Position.Y * scale),
        };
        if (keepOnTop) Overlay.RaiseFloating(window);
        dragging = handle.CapturePointer(e.Pointer);
        moved = false;
        LiveListen.Services.CardSmokeProbe.RecordDrag(window, "pressed", e, dragging, moved, start, origin);
        e.Handled = true;
    }

    private void OnMoved(object sender, PointerRoutedEventArgs e)
    {
        if (!dragging)
        {
            return;
        }
        MoveToCurrentPointer();
        LiveListen.Services.CardSmokeProbe.RecordDrag(window, "moved", e, dragging, moved, start, origin);
    }

    private void MoveToCurrentPointer()
    {
        // Once dragging starts the window origin changes. Screen coordinates
        // avoid combining queued client coordinates with that new origin.
        if (!GetCursorPos(out var p)) return;
        var dx = p.X - start.X;
        var dy = p.Y - start.Y;
        if (!moved && Math.Abs(dx) + Math.Abs(dy) < 3)
        {
            return;
        }
        moved = true;
        var to = new PointInt32(origin.X + dx, origin.Y + dy);
        to = Snap is null ? to : Snap(to);
        if (keepOnTop) Overlay.MoveFloating(window, to);
        else window.AppWindow.Move(to);
    }

    private void OnReleased(object sender, PointerRoutedEventArgs e)
    {
        if (!dragging) return;
        // A quick gesture may have no final PointerMoved callback. Apply its
        // last position before releasing capture (which can synchronously End).
        MoveToCurrentPointer();
        LiveListen.Services.CardSmokeProbe.RecordDrag(window, "released", e, dragging, moved, start, origin);
        handle.ReleasePointerCapture(e.Pointer);
        End();
    }

    private void OnCaptureLost(object sender, PointerRoutedEventArgs e)
    {
        LiveListen.Services.CardSmokeProbe.RecordDrag(window, "capture-lost", e, dragging, moved, start, origin);
        End();
    }

    private void End()
    {
        if (!dragging)
        {
            return;
        }
        dragging = false;
        if (moved)
        {
            finished(window.AppWindow.Position);
        }
        if (keepOnTop && Overlay.IsVisible(window)) Overlay.RaiseFloating(window);
    }
}
