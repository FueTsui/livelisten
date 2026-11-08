using LiveListen.Controls;

var passed = 0;
void Check(string name, Action run) { run(); Console.WriteLine("PASS " + name); passed++; }
void Require(bool condition, string message) { if (!condition) throw new InvalidOperationException(message); }
void Equal<T>(T expected, T actual) => Require(EqualityComparer<T>.Default.Equals(expected, actual), $"Expected {expected}, got {actual}");
HotkeyCaptureState Begin(params int[] held) { var state = new HotkeyCaptureState(); state.Begin(held); return state; }

Check("function keys commit once after release", () =>
{
    foreach (var key in new[] { 0x70, 0x87 })
    {
        var state = Begin();
        Equal(CaptureOutcome.None, state.Feed(key, true).Outcome);
        Equal(CaptureOutcome.None, state.Feed(key, true, repeat: true).Outcome);
        Equal(key == 0x70 ? "F1" : "F24", state.Feed(key, false).Binding);
        Equal(CaptureOutcome.None, state.Feed(key, false).Outcome);
    }
});
Check("combo waits for every modifier release", () =>
{
    var state = Begin();
    state.Feed(0xA2, true); state.Feed(0xA4, true); state.Feed(0x75, true);
    Equal(CaptureOutcome.None, state.Feed(0x75, false).Outcome);
    Equal(CaptureOutcome.None, state.Feed(0xA4, false).Outcome);
    Equal("Ctrl+Alt+F6", state.Feed(0xA2, false).Binding);
});
Check("lone right Alt and left-right physical mapping", () =>
{
    Equal(0xA5, HotkeyCaptureState.PhysicalKey(0x12, 0x38, true));
    Equal(0xA4, HotkeyCaptureState.PhysicalKey(0x12, 0x38, false));
    Equal(0xA3, HotkeyCaptureState.PhysicalKey(0x11, 0x1D, true));
    Equal(0xA1, HotkeyCaptureState.PhysicalKey(0x10, 0x36, false));
    Equal(0xA0, HotkeyCaptureState.PhysicalKey(0x10, 0x2A, true));
    Equal(0xA5, HotkeyCaptureState.PhysicalKey(0xA5, 0, false));
    var state = Begin();
    Equal(CaptureOutcome.None, state.Feed(0xA5, true).Outcome);
    Equal("RAlt", state.Feed(0xA5, false).Binding);
});
Check("AltGr is never a bare right-Alt binding", () =>
{
    var state = Begin();
    state.Feed(0xA2, true); state.Feed(0xA5, true);
    state.Feed(0xA5, false);
    Equal(CaptureOutcome.Unsupported, state.Feed(0xA2, false).Outcome);
    Require(state.Active, "Unsupported modifier chord must allow another attempt");
    state.Feed(0xA2, true); state.Feed(0xA5, true); state.Feed(0x41, true);
    state.Feed(0x41, false); state.Feed(0xA5, false);
    Equal("Ctrl+Alt+A", state.Feed(0xA2, false).Binding);
});
Check("keys held before recording cannot become standalone captures", () =>
{
    var state = Begin(0xA2);
    Equal(CaptureOutcome.None, state.Feed(0xA2, false).Outcome);
    state = Begin(0xA2);
    state.Feed(0xA5, true); state.Feed(0xA5, false);
    Equal(CaptureOutcome.Unsupported, state.Feed(0xA2, false).Outcome);
    state = Begin(0xA2);
    state.Feed(0x70, true); state.Feed(0x70, false);
    Equal("Ctrl+F1", state.Feed(0xA2, false).Binding);
    state = Begin();
    state.Feed(0x41, true, repeat: true);
    Equal(CaptureOutcome.None, state.Feed(0x41, false).Outcome);
    state.Feed(0x71, true);
    Equal("F2", state.Feed(0x71, false).Binding);
});
Check("escape and focus cancellation cannot commit a pending press", () =>
{
    var state = Begin(); state.Feed(0x76, true);
    Equal(CaptureOutcome.Cancelled, state.Feed(0x1B, true).Outcome);
    Equal(CaptureOutcome.None, state.Feed(0x76, false).Outcome);
    state.Begin(); state.Feed(0xA5, true); state.Cancel();
    Equal(CaptureOutcome.None, state.Feed(0xA5, false).Outcome);
    state.Begin(); state.Feed(0x77, true); Equal("F8", state.Feed(0x77, false).Binding);
});
Check("unsupported key cannot turn into a modifier shortcut", () =>
{
    var state = Begin(); state.Feed(0xA5, true);
    Equal(CaptureOutcome.Unsupported, state.Feed(0xFF, true).Outcome);
    state.Feed(0xFF, false);
    Equal(CaptureOutcome.None, state.Feed(0xA5, false).Outcome);
    state.Feed(0x78, true); Equal("F9", state.Feed(0x78, false).Binding);
});
Check("canonical names catch aliases and reordered modifiers without merging sides", () =>
{
    Equal("Ctrl+Alt+F6", HotkeyCaptureState.Canonical("alt + CONTROL + f6"));
    Equal("RAlt", HotkeyCaptureState.Canonical("rightalt"));
    Equal("RCtrl", HotkeyCaptureState.Canonical("rcontrol"));
    Require(HotkeyCaptureState.Canonical("LAlt") != HotkeyCaptureState.Canonical("RAlt"), "Side-specific bindings were merged");
    Equal<string?>(null, HotkeyCaptureState.Canonical("Ctrl+RAlt"));
    Equal<string?>(null, HotkeyCaptureState.Canonical("F25"));
    Equal<string?>(null, HotkeyCaptureState.Canonical("Ctrl++A"));
    Equal("", HotkeyCaptureState.Canonical(" "));
});
Check("rapid local edits conflict before a backend snapshot catches up", () =>
{
    var current = new Dictionary<string, string> { ["hold"] = "RCtrl", ["toggle"] = "F5", ["note"] = "RAlt" };
    current["toggle"] = "Ctrl+Alt+F6";
    Require(HotkeyCaptureState.HasDuplicate("alt+control+f6", "note", current), "Latest local draft was not considered");
    Require(!HotkeyCaptureState.HasDuplicate("Ctrl+Alt+F6", "toggle", current), "The field conflicts with itself");
    current["toggle"] = "";
    Require(!HotkeyCaptureState.HasDuplicate("Ctrl+Alt+F6", "note", current), "Cleared local shortcut still conflicts");
});
Console.WriteLine($"Passed {passed} hotkey capture checks.");
