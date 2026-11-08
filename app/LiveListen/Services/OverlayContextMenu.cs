using System.Runtime.InteropServices;
using LiveListen.Controls;
using LiveListen.Interop;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Input;
using Windows.Foundation;
using Windows.UI;
using static LiveListen.Interop.Win32;

namespace LiveListen.Services;

/// <summary>
/// The context menu of the floating overlays, with a translucent background (text stays opaque).
/// The overlays never activate, so XAML light dismiss cannot see clicks in other apps. The menu
/// opens without taking the keyboard focus, and closes on any key press or on a click anywhere
/// outside it; that key or click still reaches the app it was meant for.
/// </summary>
internal sealed class OverlayContextMenu : IDisposable
{
    private MenuFlyout? menu;
    private bool disposed;

    internal bool IsOpen => menu is not null;

    public void Show(Window owner, FrameworkElement anchor, IReadOnlyList<TrayItem> items, bool dark)
    {
        if (disposed || menu is not null || items.Count == 0 || anchor.XamlRoot is null) return;
        var previous = GetForegroundWindow();
        var hwnd = Overlay.Handle(owner);
        var queue = owner.DispatcherQueue;
        var foreground = Color.FromArgb(255, dark ? (byte)250 : (byte)24, dark ? (byte)250 : (byte)24, dark ? (byte)250 : (byte)24);
        // The independent popup backdrop is transparent. Only this brush carries alpha;
        // menu labels and interaction feedback retain their own normal opacity.
        var background = dark ? Color.FromArgb(215, 32, 32, 36) : Color.FromArgb(225, 249, 249, 252);
        var style = new Style(typeof(MenuFlyoutPresenter));
        style.Setters.Add(new Setter(Control.BackgroundProperty, new SolidColorBrush(background)));
        style.Setters.Add(new Setter(Control.ForegroundProperty, new SolidColorBrush(foreground)));
        style.Setters.Add(new Setter(Control.BorderBrushProperty, new SolidColorBrush(Color.FromArgb(55, dark ? (byte)255 : (byte)0, dark ? (byte)255 : (byte)0, dark ? (byte)255 : (byte)0))));
        style.Setters.Add(new Setter(Control.BorderThicknessProperty, new Thickness(1)));
        style.Setters.Add(new Setter(Control.CornerRadiusProperty, new CornerRadius(8)));
        // User-visible width is in screen pixels, not 180 DIPs (270 px at 150%).
        var width = 180d / Overlay.Scale(owner);
        style.Setters.Add(new Setter(FrameworkElement.MinWidthProperty, width));
        style.Setters.Add(new Setter(FrameworkElement.MaxWidthProperty, width));
        style.Setters.Add(new Setter(FrameworkElement.WidthProperty, width));
        style.Setters.Add(new Setter(FrameworkElement.RequestedThemeProperty, dark ? ElementTheme.Dark : ElementTheme.Light));
        var current = new MenuFlyout
        {
            MenuFlyoutPresenterStyle = style,
            SystemBackdrop = new TransparentBackdrop(),
            ShouldConstrainToRootBounds = false,
        };
        Action? selected = null;
        MenuFlyoutPresenter? presenter = null;
        void DismissAfterInput() => queue.TryEnqueue(() =>
        {
            if (ReferenceEquals(menu, current)) current.Hide();
        });
        KeyEventHandler keyDown = (_, _) => DismissAfterInput();
        PointerEventHandler pointerReleased = (_, _) => DismissAfterInput();
        TypedEventHandler<object, WindowActivatedEventArgs> activation = (_, e) =>
        {
            if (e.WindowActivationState == WindowActivationState.Deactivated) DismissAfterInput();
        };
        current.Opened += (_, _) =>
        {
            for (DependencyObject? node = current.Items[0]; node is not null; node = VisualTreeHelper.GetParent(node))
            {
                if (node is MenuFlyoutPresenter found) { presenter = found; break; }
            }
            presenter?.AddHandler(UIElement.KeyDownEvent, keyDown, true);
            presenter?.AddHandler(UIElement.PointerReleasedEvent, pointerReleased, true);
            owner.Activated += activation;
        };
        foreach (var item in items)
        {
            if (item == TrayItem.Separator)
            {
                current.Items.Add(new MenuFlyoutSeparator());
                continue;
            }
            var button = new MenuFlyoutItem
            {
                Text = item.Text,
                IsEnabled = item.Enabled,
                Foreground = new SolidColorBrush(foreground),
                Opacity = 1,
            };
            if (item.Checked) button.Icon = new FontIcon { Glyph = "", FontSize = 12 };
            button.Click += (_, _) => { selected = item.Run; current.Hide(); };
            current.Items.Add(button);
        }
        current.Closed += (_, _) =>
        {
            DismissHooks.Stop();
            owner.Activated -= activation;
            presenter?.RemoveHandler(UIElement.KeyDownEvent, keyDown);
            presenter?.RemoveHandler(UIElement.PointerReleasedEvent, pointerReleased);
            presenter = null;
            if (ReferenceEquals(menu, current)) menu = null;
            RestoreTypingTarget(hwnd, previous);
            var action = selected;
            if (action is not null && !disposed)
                queue.TryEnqueue(DispatcherQueuePriority.Normal, () => { if (!disposed) action(); });
        };
        GetCursorPos(out var cursor);
        ScreenToClient(hwnd, ref cursor);
        var scale = Overlay.Scale(owner);
        var point = new Point(cursor.X / scale, cursor.Y / scale);
        if (owner.Content is UIElement root) point = root.TransformToVisual(anchor).TransformPoint(point);
        menu = current;
        try
        {
            var hooked = DismissHooks.Start(DismissAfterInput);
            if (!hooked)
            {
                // Without the hooks, XAML light dismiss is the only way out, and it needs the
                // overlay to own the keyboard focus, as a native context menu does.
                Overlay.BringToFront(owner);
            }
            Overlay.RaiseFloating(owner);
            current.ShowAt(anchor, new FlyoutShowOptions
            {
                Position = point,
                ShowMode = hooked ? FlyoutShowMode.Transient : FlyoutShowMode.Standard,
            });
        }
        catch
        {
            DismissHooks.Stop();
            menu = null;
            RestoreTypingTarget(hwnd, previous);
            throw;
        }
    }

    public void Dispose()
    {
        disposed = true;
        DismissHooks.Stop();
        menu?.Hide();
        menu = null;
    }

    /// <summary>
    /// Gives the keyboard back to the app the user was typing in when the overlay or the menu
    /// took it (a click on a menu item can activate the menu's own window). An outside click
    /// that chose a new target, including one of this app's normal windows, keeps it.
    /// </summary>
    private static void RestoreTypingTarget(nint owner, nint previous)
    {
        var front = GetForegroundWindow();
        if (previous != 0 && previous != owner && !DismissHooks.IsMenuWindow(previous)
            && (front == owner || DismissHooks.IsMenuWindow(front)))
            SetForegroundWindow(previous);
    }

    /// <summary>
    /// Low-level hooks that exist only while the menu is open. They run on the UI thread, do a
    /// few window lookups, queue the dismissal and pass every event on unchanged.
    /// </summary>
    private static unsafe class DismissHooks
    {
        /// <summary>Class of every <see cref="Window"/>; menus and other popups use their own.</summary>
        private const string MainWindowClass = "WinUIDesktopWin32WindowClass";
        private static readonly uint ProcessId = (uint)Environment.ProcessId;
        private static nint keyboard, mouse;
        private static Action? dismiss;

        public static bool Start(Action onDismiss)
        {
            Stop();
            var module = GetModuleHandle(0);
            keyboard = SetWindowsHookEx(WH_KEYBOARD_LL, &KeyboardProc, module, 0);
            mouse = SetWindowsHookEx(WH_MOUSE_LL, &MouseProc, module, 0);
            if (keyboard == 0 || mouse == 0)
            {
                Stop();
                return false;
            }
            dismiss = onDismiss;
            return true;
        }

        public static void Stop()
        {
            if (keyboard != 0) UnhookWindowsHookEx(keyboard);
            if (mouse != 0) UnhookWindowsHookEx(mouse);
            keyboard = mouse = 0;
            dismiss = null;
        }

        /// <summary>A top-level window of this process that is not a normal window: the menu.</summary>
        public static bool IsMenuWindow(nint window)
        {
            if (window == 0) return false;
            GetWindowThreadProcessId(window, out var pid);
            if (pid != ProcessId) return false;
            var name = stackalloc char[64];
            var length = GetClassName(window, name, 64);
            return length > 0 && !new ReadOnlySpan<char>(name, length).SequenceEqual(MainWindowClass);
        }

        [UnmanagedCallersOnly]
        private static nint KeyboardProc(int code, nint wparam, nint lparam)
        {
            try
            {
                var message = (uint)wparam;
                // Keys typed by dictation carry the core's marker and keep the menu open.
                if (code == HC_ACTION && (message == WM_KEYDOWN || message == WM_SYSKEYDOWN)
                    && ((KBDLLHOOKSTRUCT*)lparam)->dwExtraInfo != CoreInputMarker)
                    dismiss?.Invoke();
            }
            catch
            {
                // Never let an exception unwind into the system's input path.
            }
            return CallNextHookEx(0, code, wparam, lparam);
        }

        [UnmanagedCallersOnly]
        private static nint MouseProc(int code, nint wparam, nint lparam)
        {
            try
            {
                var message = (uint)wparam;
                if (code == HC_ACTION && message is WM_LBUTTONDOWN or WM_RBUTTONDOWN or WM_MBUTTONDOWN or WM_XBUTTONDOWN)
                {
                    var info = (MSLLHOOKSTRUCT*)lparam;
                    if (info->dwExtraInfo != CoreInputMarker)
                    {
                        var target = GetAncestor(WindowFromPoint(info->pt), GA_ROOT);
                        if (!IsMenuWindow(target)) dismiss?.Invoke();
                    }
                }
            }
            catch
            {
                // Never let an exception unwind into the system's input path.
            }
            return CallNextHookEx(0, code, wparam, lparam);
        }
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ScreenToClient(nint hwnd, ref POINT point);
}
