using System.Runtime.InteropServices;
using LiveListen.Interop;
using Microsoft.UI.Dispatching;
using static LiveListen.Interop.Win32;

namespace LiveListen.Services;

/// <summary>One item of the notification-area menu.</summary>
internal sealed record TrayItem(string Text, Action? Run, bool Checked = false, bool Enabled = true)
{
    public static TrayItem Separator { get; } = new("-", null);
}

/// <summary>
/// Notification-area icon backed by a hidden Win32 window. Messages arrive on the UI thread.
/// </summary>
internal sealed unsafe class TrayIcon : IDisposable
{
    private const uint CallbackMessage = WM_APP + 1;
    private const uint MenuMessage = WM_APP + 2;
    private const string ClassName = "LiveListen.TrayWindow";
    private static TrayIcon? instance;

    private readonly nint hwnd;
    private readonly uint taskbarCreated;
    private readonly DispatcherQueue dispatcher;
    private nint icon;
    private string tip;
    private bool added;
    private bool menuOpen;
    private bool menuCancelling;
    private bool leftPressed;
    private bool disposed;
    private IReadOnlyList<TrayItem>? pendingMenu;
    private POINT pendingPoint;

    /// <summary>Single left click or keyboard selection on the icon.</summary>
    public event Action? Clicked;

    /// <summary>Windows is shutting down or signing out; raised on the UI thread and must return quickly.</summary>
    public event Action? SessionEnding;

    /// <summary>The display layout or resolution changed.</summary>
    public event Action? DisplayChanged;

    /// <summary>False while the session is locked or the display is off: nothing can be seen.</summary>
    public event Action<bool>? ScreenActiveChanged;

    private bool locked;
    private bool displayOff;
    private nint powerNotification;
    private bool sessionNotification;

    /// <summary>Builds the menu each time it opens, so checks are current.</summary>
    public Func<IReadOnlyList<TrayItem>>? Menu { get; set; }

    public TrayIcon(string iconPath, string tip)
    {
        instance = this;
        dispatcher = DispatcherQueue.GetForCurrentThread();
        this.tip = tip;
        var module = GetModuleHandle(0);
        fixed (char* name = ClassName)
        {
            var wc = new WNDCLASSEXW
            {
                cbSize = (uint)sizeof(WNDCLASSEXW),
                lpfnWndProc = &WndProc,
                hInstance = module,
                lpszClassName = name,
            };
            RegisterClassEx(&wc);
        }
        hwnd = CreateWindowEx((uint)WS_EX_TOOLWINDOW, ClassName, "Live Listen", 0, 0, 0, 0, 0, 0, 0, module, 0);
        taskbarCreated = RegisterWindowMessage("TaskbarCreated");
        icon = LoadImage(0, iconPath, IMAGE_ICON, GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON), LR_LOADFROMFILE);
        try
        {
            SetPreferredAppMode(1); // Allow dark menus when Windows uses dark mode.
            FlushMenuThemes();
        }
        catch (EntryPointNotFoundException)
        {
        }
        Add();
        sessionNotification = WTSRegisterSessionNotification(hwnd, 0);
        var displayState = new Guid("6fe69556-704a-47a0-8f24-c28d936fda47"); // GUID_CONSOLE_DISPLAY_STATE
        powerNotification = RegisterPowerSettingNotification(hwnd, ref displayState, 0);
    }

    private void SetScreen(bool? nowLocked, bool? nowOff)
    {
        var before = !locked && !displayOff;
        locked = nowLocked ?? locked;
        displayOff = nowOff ?? displayOff;
        var after = !locked && !displayOff;
        if (before != after) dispatcher.TryEnqueue(() => { if (!disposed) ScreenActiveChanged?.Invoke(after); });
    }

    public void SetTip(string text)
    {
        tip = text;
        var data = Data(NIF_TIP | NIF_SHOWTIP);
        Shell_NotifyIcon(NIM_MODIFY, &data);
    }

    /// <summary>Shows a quiet notification balloon (respects Focus Assist).</summary>
    public void Notify(string title, string text)
    {
        var data = Data(NIF_INFO);
        data.dwInfoFlags = NIIF_INFO | NIIF_RESPECT_QUIET_TIME;
        Copy(title, data.szInfoTitle, 64);
        Copy(text, data.szInfo, 256);
        Shell_NotifyIcon(NIM_MODIFY, &data);
    }

    private void Add()
    {
        var data = Data(NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP);
        added = Shell_NotifyIcon(NIM_ADD, &data);
        data.uVersion = NOTIFYICON_VERSION_4;
        Shell_NotifyIcon(NIM_SETVERSION, &data);
    }

    private NOTIFYICONDATAW Data(uint flags)
    {
        var data = new NOTIFYICONDATAW
        {
            cbSize = (uint)sizeof(NOTIFYICONDATAW),
            hWnd = hwnd,
            uID = 1,
            uFlags = flags,
            uCallbackMessage = CallbackMessage,
            hIcon = icon,
        };
        Copy(tip, data.szTip, 128);
        return data;
    }

    private static void Copy(string text, char* target, int capacity)
    {
        var n = Math.Min(text.Length, capacity - 1);
        for (var i = 0; i < n; i++)
        {
            target[i] = text[i];
        }
        target[n] = '\0';
    }

    public void ShowMenu(IReadOnlyList<TrayItem>? items = null)
    {
        if (menuOpen || pendingMenu is not null) return;
        items ??= Menu?.Invoke();
        if (items is null || items.Count == 0)
        {
            return;
        }
        // Start the modal loop from a native window message, after the XAML input
        // and DispatcherQueue callback have returned. Both disallow reentrant UI work.
        pendingMenu = items;
        GetCursorPos(out pendingPoint);
        if (!PostMessage(hwnd, MenuMessage, 0, 0)) pendingMenu = null;
    }

    private void TrackMenu(IReadOnlyList<TrayItem> items, POINT p)
    {
        var menu = CreatePopupMenu();
        if (menu == 0) return;
        for (var i = 0; i < items.Count; i++)
        {
            var item = items[i];
            if (item == TrayItem.Separator)
            {
                AppendMenu(menu, MF_SEPARATOR, 0, null);
                continue;
            }
            var flags = MF_STRING | (item.Checked ? MF_CHECKED : 0) | (item.Enabled ? 0 : MF_GRAYED);
            AppendMenu(menu, flags, (nuint)(i + 1), item.Text);
        }
        var previous = GetForegroundWindow();
        // Required so the menu closes when the user clicks elsewhere.
        SetForegroundWindow(hwnd);
        int chosen;
        menuOpen = true;
        try
        {
            chosen = TrackPopupMenuEx(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, p.X, p.Y, hwnd, 0);
        }
        finally
        {
            menuOpen = false;
            menuCancelling = false;
            PostMessage(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
        }
        // Restore the typing target only if menu ownership still has focus. An outside
        // click that already activated another app must keep the user's new focus.
        if (GetForegroundWindow() == hwnd && previous != 0 && previous != hwnd)
            SetForegroundWindow(previous);
        if (chosen > 0 && chosen <= items.Count)
        {
            var action = items[chosen - 1].Run;
            if (action is not null) dispatcher.TryEnqueue(() => action());
        }
    }

    private bool Handle(uint msg, nint wParam, nint lParam)
    {
        if (msg == MenuMessage)
        {
            var items = pendingMenu;
            pendingMenu = null;
            if (items is not null) TrackMenu(items, pendingPoint);
            return true;
        }
        // A no-activate overlay's menu must also end when another process takes
        // the foreground. TrackPopupMenu alone can retain its modal loop here.
        // These are owner-window messages, not hooks or periodic focus checks.
        if (menuOpen && !menuCancelling && ((msg == 0x001C && wParam == 0) // WM_ACTIVATEAPP
            || (msg == 0x0006 && (wParam & 0xFFFF) == 0) // WM_ACTIVATE / WA_INACTIVE
            || msg == 0x001F)) // WM_CANCELMODE
        {
            // EndMenu may synchronously send WM_CANCELMODE back to this owner.
            menuCancelling = true;
            EndMenu();
        }
        if (msg == CallbackMessage)
        {
            switch ((uint)(lParam & 0xFFFF))
            {
                case WM_LBUTTONDOWN:
                case 0x0203: // WM_LBUTTONDBLCLK replaces the second button-down.
                    leftPressed = true;
                    return true;
                case WM_LBUTTONUP:
                case NIN_SELECT: // Explorer can report both for the same release.
                    if (leftPressed)
                    {
                        leftPressed = false;
                        DispatchClick();
                    }
                    return true;
                case NIN_KEYSELECT:
                    leftPressed = false;
                    DispatchClick();
                    return true;
                case WM_CONTEXTMENU:
                    leftPressed = false;
                    ShowMenu();
                    return true;
            }
            return true;
        }
        if (msg == WM_ENDSESSION && wParam != 0)
        {
            // The process ends when this returns; nothing may be queued for later.
            SessionEnding?.Invoke();
            return false;
        }
        if (msg == 0x02B1) // WM_WTSSESSION_CHANGE
        {
            if (wParam == 7) SetScreen(true, null); // WTS_SESSION_LOCK
            else if (wParam == 8) SetScreen(false, null); // WTS_SESSION_UNLOCK
            return false;
        }
        if (msg == 0x0218 && wParam == 0x8013 && lParam != 0) // WM_POWERBROADCAST / PBT_POWERSETTINGCHANGE
        {
            // POWERBROADCAST_SETTING: GUID, DataLength, Data. 0 = display off, 1 = on, 2 = dimmed.
            var length = *(uint*)(lParam + 16);
            if (length >= 4) SetScreen(null, *(uint*)(lParam + 20) == 0);
            return false;
        }
        if (msg == WM_DISPLAYCHANGE)
        {
            dispatcher.TryEnqueue(() => { if (!disposed) DisplayChanged?.Invoke(); });
            return false;
        }
        if (msg == taskbarCreated && taskbarCreated != 0)
        {
            // Explorer restarted: the icon must be added again.
            leftPressed = false;
            Add();
            return true;
        }
        return false;
    }

    private void DispatchClick()
        => dispatcher.TryEnqueue(() => { if (!disposed) Clicked?.Invoke(); });

    [UnmanagedCallersOnly]
    private static nint WndProc(nint hwnd, uint msg, nint wParam, nint lParam)
    {
        var self = instance;
        if (self is not null && hwnd == self.hwnd)
        {
            try
            {
                if (self.Handle(msg, wParam, lParam))
                {
                    return 0;
                }
            }
            catch (Exception)
            {
                // Never let an exception cross the native boundary.
            }
        }
        return DefWindowProc(hwnd, msg, wParam, lParam);
    }

    public void Dispose()
    {
        disposed = true;
        if (sessionNotification) WTSUnRegisterSessionNotification(hwnd);
        if (powerNotification != 0) UnregisterPowerSettingNotification(powerNotification);
        if (added)
        {
            var data = Data(0);
            Shell_NotifyIcon(NIM_DELETE, &data);
            added = false;
        }
        if (icon != 0)
        {
            DestroyIcon(icon);
            icon = 0;
        }
        DestroyWindow(hwnd);
        instance = null;
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool EndMenu();

    [DllImport("wtsapi32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WTSRegisterSessionNotification(nint hwnd, uint flags);

    [DllImport("wtsapi32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WTSUnRegisterSessionNotification(nint hwnd);

    [DllImport("user32.dll")]
    private static extern nint RegisterPowerSettingNotification(nint recipient, ref Guid setting, uint flags);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool UnregisterPowerSettingNotification(nint handle);
}
