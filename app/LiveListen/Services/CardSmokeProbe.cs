using System.Runtime.InteropServices;
using System.Text.Json;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using LiveListen.Controls;

namespace LiveListen.Services;

/// <summary>Opt-in diagnostics for isolated card smoke runs; never enabled for user data.</summary>
internal static class CardSmokeProbe
{
    private static readonly Dictionary<Window, string> paths = [];

    public static bool Enabled => EngineHost.TestConfigDir is not null
        && Environment.GetCommandLineArgs().Contains("--card-smoke", StringComparer.OrdinalIgnoreCase);

    public static void Attach(Window window, FrameworkElement root, ulong id, Func<bool> editing)
    {
        if (!Enabled) return;
        // Tool windows are intentionally absent from task switchers in normal use. Test
        // runs expose the exact same card so the desktop automation can select it by ID.
        window.AppWindow.IsShownInSwitchers = true;
        var hwnd = Overlay.Handle(window);
        var ex = LiveListen.Interop.Win32.GetWindowLongPtr(hwnd, LiveListen.Interop.Win32.GWL_EXSTYLE);
        LiveListen.Interop.Win32.SetWindowLongPtr(hwnd, LiveListen.Interop.Win32.GWL_EXSTYLE,
            (ex | LiveListen.Interop.Win32.WS_EX_APPWINDOW) & ~LiveListen.Interop.Win32.WS_EX_TOOLWINDOW);
        window.Title = $"LiveListen card smoke {id}";
        var path = Path.Combine(EngineHost.TestConfigDir!, $"card-smoke-{id}.jsonl");
        paths[window] = path;
        var timer = window.DispatcherQueue.CreateTimer();
        timer.Interval = TimeSpan.FromMilliseconds(100);
        string? pointerSource = null;
        string? pointerPath = null;
        root.AddHandler(UIElement.PointerMovedEvent,
            new Microsoft.UI.Xaml.Input.PointerEventHandler((_, e) =>
            {
                pointerSource = e.OriginalSource?.GetType().Name;
                var pathParts = new List<string>();
                for (var node = e.OriginalSource as DependencyObject; node is not null; node = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetParent(node))
                {
                    var cursorValue = node is UIElement ? typeof(UIElement).GetProperty("ProtectedCursor",
                        System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic)?.GetValue(node) : null;
                    var cursorName = cursorValue is Microsoft.UI.Input.InputSystemCursor system ? system.CursorShape.ToString() : cursorValue?.GetType().Name;
                    pathParts.Add(node.GetType().Name + ":" + cursorName);
                }
                pointerPath = string.Join(" > ", pathParts);
            }), true);
        string? previous = null;
        var last = DateTimeOffset.MinValue;
        timer.Tick += (_, _) =>
        {
            var cursor = new CursorInfo { Size = Marshal.SizeOf<CursorInfo>() };
            if (!GetCursorInfo(ref cursor)) return;
            var pos = window.AppWindow.Position;
            var size = window.AppWindow.Size;
            if (cursor.Point.X < pos.X || cursor.Point.X >= pos.X + size.Width
                || cursor.Point.Y < pos.Y || cursor.Point.Y >= pos.Y + size.Height) return;
            var under = WindowFromPoint(cursor.Point);
            var shape = cursor.Cursor == LoadCursor(0, 32512) ? "arrow"
                : cursor.Cursor == LoadCursor(0, 32513) ? "ibeam" : cursor.Cursor.ToString();
            var focus = root.XamlRoot is null ? null : Microsoft.UI.Xaml.Input.FocusManager.GetFocusedElement(root.XamlRoot)?.GetType().Name;
            var state = JsonSerializer.Serialize(new
            {
                shape, cursorX = cursor.Point.X, cursorY = cursor.Point.Y,
                under = under.ToInt64(), ancestor = GetAncestor(under, 2).ToInt64(),
                own = Overlay.Handle(window).ToInt64(), foreground = GetForegroundWindow().ToInt64(),
                x = pos.X, y = pos.Y, width = size.Width, height = size.Height, focus, pointerSource, pointerPath, editing = editing(),
            });
            var now = DateTimeOffset.UtcNow;
            if (state == previous && (now - last).TotalSeconds < 1) return;
            previous = state;
            last = now;
            try { File.AppendAllText(path, $"{{\"at\":\"{now:O}\",\"state\":{state}}}\n"); }
            catch (IOException) { /* Diagnostics cannot interfere with editing. */ }
        };
        window.Closed += (_, _) => { timer.Stop(); paths.Remove(window); };
        timer.Start();
    }

    public static void RecordDrag(Window window, string stage, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs args,
        bool dragging, bool moved, LiveListen.Interop.POINT start, Windows.Graphics.PointInt32 origin)
    {
        if (!Enabled || !paths.TryGetValue(window, out var path)) return;
        var point = args.GetCurrentPoint(null);
        LiveListen.Interop.Win32.GetCursorPos(out var cursor);
        var position = window.AppWindow.Position;
        var entry = JsonSerializer.Serialize(new
        {
            at = DateTimeOffset.UtcNow, drag = stage, point.PointerId, point.Timestamp,
            eventX = point.Position.X, eventY = point.Position.Y, generated = args.IsGenerated,
            leftButton = point.Properties.IsLeftButtonPressed, source = args.OriginalSource?.GetType().Name,
            cursorX = cursor.X, cursorY = cursor.Y, dragging, moved,
            startX = start.X, startY = start.Y, originX = origin.X, originY = origin.Y,
            windowX = position.X, windowY = position.Y,
        });
        try { File.AppendAllText(path, entry + "\n"); }
        catch (IOException) { /* Diagnostics cannot interfere with input. */ }
        catch (UnauthorizedAccessException) { /* Diagnostics cannot interfere with input. */ }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)]
    private struct CursorInfo { public int Size; public uint Flags; public nint Cursor; public Point Point; }
    [DllImport("user32.dll")] [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetCursorInfo(ref CursorInfo info);
    [DllImport("user32.dll", EntryPoint = "LoadCursorW")]
    private static extern nint LoadCursor(nint instance, nint name);
    [DllImport("user32.dll")]
    private static extern nint WindowFromPoint(Point point);
    [DllImport("user32.dll")]
    private static extern nint GetAncestor(nint window, uint flags);
    [DllImport("user32.dll")]
    private static extern nint GetForegroundWindow();
}
