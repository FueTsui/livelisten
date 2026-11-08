namespace LiveListen.Controls;

/// <summary>One focused shortcut gesture. No hooks, polling, UI, or persisted state.</summary>
internal sealed class HotkeyCaptureState
{
    private readonly HashSet<int> down = [];
    private readonly HashSet<int> modifiersPressed = [];
    private string? candidate;
    private bool usedOrdinaryKey;
    private bool multipleModifiers;
    public bool Active { get; private set; }
    public string Preview { get; private set; } = "";

    public void Begin(IEnumerable<int>? heldModifiers = null)
    {
        down.Clear();
        modifiersPressed.Clear();
        if (heldModifiers is not null) foreach (var key in heldModifiers.Where(IsModifier)) down.Add(key);
        candidate = null;
        usedOrdinaryKey = false;
        multipleModifiers = down.Count > 1;
        Preview = "";
        Active = true;
    }

    public void Cancel() { Active = false; down.Clear(); candidate = null; Preview = ""; }

    public CaptureResult Feed(int key, bool pressed, bool repeat = false)
    {
        if (!Active) return default;
        if (pressed)
        {
            if (key == 0x1B) { Cancel(); return new(CaptureOutcome.Cancelled); }
            if (repeat || !down.Add(key)) return default;
            if (IsModifier(key))
            {
                modifiersPressed.Add(key);
                if (down.Count(IsModifier) > 1) multipleModifiers = true;
            }
            else
            {
                usedOrdinaryKey = true;
                if (candidate is null)
                {
                    if (KeyName(key) is not { } name) return new(CaptureOutcome.Unsupported);
                    candidate = ModifierPrefix(down) + name;
                }
            }
            Preview = candidate ?? ModifierPrefix(down).TrimEnd('+');
            return default;
        }
        if (!down.Remove(key)) return default;
        if (down.Count != 0) return default;
        if (candidate is null && !usedOrdinaryKey)
        {
            // Multiple modifiers without an ordinary key are not a binding. This
            // also keeps AltGr's synthetic LCtrl+RAlt from becoming a bare RAlt.
            if (modifiersPressed.Count == 1 && !multipleModifiers) candidate = KeyName(modifiersPressed.Single());
        }
        if (candidate is { } binding)
        {
            Cancel();
            return new(CaptureOutcome.Completed, binding);
        }
        var unsupported = multipleModifiers && !usedOrdinaryKey;
        modifiersPressed.Clear();
        usedOrdinaryKey = false;
        multipleModifiers = false;
        Preview = "";
        return unsupported ? new(CaptureOutcome.Unsupported) : default;
    }

    public static int PhysicalKey(int key, uint scanCode, bool extended) => key switch
    {
        0x10 => scanCode == 0x36 ? 0xA1 : 0xA0,
        0x11 => extended ? 0xA3 : 0xA2,
        0x12 => extended ? 0xA5 : 0xA4,
        _ => key,
    };

    public static bool IsModifier(int key) => key is >= 0xA0 and <= 0xA5 or 0x5B or 0x5C;

    private static string ModifierPrefix(IEnumerable<int> keys)
    {
        var set = keys.ToHashSet();
        return (set.Overlaps([0xA2, 0xA3]) ? "Ctrl+" : "")
            + (set.Overlaps([0xA4, 0xA5]) ? "Alt+" : "")
            + (set.Overlaps([0xA0, 0xA1]) ? "Shift+" : "")
            + (set.Overlaps([0x5B, 0x5C]) ? "Win+" : "");
    }

    public static string? KeyName(int key)
    {
        if (key is >= 0x30 and <= 0x39 or >= 0x41 and <= 0x5A) return ((char)key).ToString();
        if (key is >= 0x70 and <= 0x87) return "F" + (key - 0x6F);
        if (key is >= 0x60 and <= 0x69) return "Num" + (key - 0x60);
        return key switch
        {
            0x08 => "Backspace", 0x09 => "Tab", 0x0D => "Enter", 0x13 => "Pause", 0x14 => "CapsLock",
            0x1B => "Esc", 0x20 => "Space", 0x21 => "PageUp", 0x22 => "PageDown", 0x23 => "End",
            0x24 => "Home", 0x25 => "Left", 0x26 => "Up", 0x27 => "Right", 0x28 => "Down",
            0x2C => "PrintScreen", 0x2D => "Insert", 0x2E => "Delete", 0x5B => "LWin", 0x5C => "RWin",
            0x5D => "Menu", 0x6A => "NumMultiply", 0x6B => "NumAdd", 0x6D => "NumSubtract",
            0x6E => "NumDecimal", 0x6F => "NumDivide", 0x90 => "NumLock", 0x91 => "ScrollLock",
            0xA0 => "LShift", 0xA1 => "RShift", 0xA2 => "LCtrl", 0xA3 => "RCtrl", 0xA4 => "LAlt", 0xA5 => "RAlt",
            0xBA => ";", 0xBB => "=", 0xBC => ",", 0xBD => "-", 0xBE => ".", 0xBF => "/",
            0xC0 => "`", 0xDB => "[", 0xDC => "\\", 0xDD => "]", 0xDE => "'", _ => null,
        };
    }

    public static string? Canonical(string binding)
    {
        if (string.IsNullOrWhiteSpace(binding)) return "";
        var parts = binding.Split('+', StringSplitOptions.TrimEntries);
        if (parts.Any(string.IsNullOrEmpty)) return null;
        var modifiers = new HashSet<int>();
        foreach (var part in parts[..^1])
        {
            var key = part.ToLowerInvariant() switch
            {
                "ctrl" or "control" => 0xA2, "alt" => 0xA4, "shift" => 0xA0,
                "win" or "windows" => 0x5B, _ => 0,
            };
            if (key == 0) return null;
            modifiers.Add(key);
        }
        var last = parts[^1].ToLowerInvariant() switch
        {
            "escape" => "Esc", "return" => "Enter", "pgup" => "PageUp", "pgdn" => "PageDown",
            "rightctrl" or "rcontrol" => "RCtrl", "leftctrl" or "lcontrol" => "LCtrl",
            "rightalt" or "altgr" => "RAlt", "leftalt" => "LAlt", _ => parts[^1],
        };
        var code = Enumerable.Range(1, 254).FirstOrDefault(v => string.Equals(KeyName(v), last, StringComparison.OrdinalIgnoreCase));
        if (code == 0 || IsModifier(code) && modifiers.Count > 0) return null;
        return ModifierPrefix(modifiers) + KeyName(code);
    }

    public static bool HasDuplicate(string binding, string field, IEnumerable<KeyValuePair<string, string>> current) =>
        Canonical(binding) is { Length: > 0 } canonical
        && current.Any(pair => pair.Key != field && Canonical(pair.Value) == canonical);
}

internal enum CaptureOutcome { None, Completed, Cancelled, Unsupported }
internal readonly record struct CaptureResult(CaptureOutcome Outcome, string? Binding = null);
