using System.Text.Json.Nodes;
using LiveListen.Services;

// Offline behavior checks: no UI, native DLL, credentials or user settings are touched.
var checks = new (string Name, Func<Task> Run)[]
{
    ("shutdown waits for accepted work that has not started", QueuedCallIsDrained),
    ("shutdown rejects new calls", ShutdownRejectsNewCalls),
    ("a failed call releases its shutdown lease", FailedCallReleases),
    ("failed shutdown can resume calls and retry drains new work", FailedShutdownCanResume),
    ("settings edits coalesce and old replies cannot overwrite new edits", SettingsCoalesce),
    ("failed settings remain dirty and can be retried", FailedSettingsRetry),
    ("a newer snapshot is tried after an older one fails", NewerSnapshotAfterFailure),
    ("an exception cannot wedge the settings save loop", SettingsExceptionRecovery),
    ("voice responds only to live levels and releases stale input", VoiceLevels),
    ("hidden or reduced-motion voice stops and clears old input", VoiceVisibility),
    ("wheel sizing follows fine deltas linearly, reverses and clamps", VoiceWheel),
    ("unwritten clipboard copies latest suffix once and recovers from a busy clipboard", ClipboardRetry),
    ("new text replaces pending clipboard retries without stale overwrite", ClipboardLatest),
    ("new dictation cancels clipboard retries and failure never claims copied", ClipboardCancelAndFailure),
    ("surface edits invalidate only their readers and section resets refresh all descendants", SurfaceRefreshRouting),
    ("a broken error observer cannot strand saves or shutdown", BrokenSaveObserver),
};
foreach (var (name, run) in checks)
{
    await run().WaitAsync(TimeSpan.FromSeconds(5));
    Console.WriteLine("PASS " + name);
}
Console.WriteLine($"{checks.Length} checks passed");

static void Check(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
}

static TaskCompletionSource<T> Signal<T>() => new(TaskCreationOptions.RunContinuationsAsynchronously);

static async Task BrokenSaveObserver()
{
    var accept = false;
    var queue = new SettingsSaveQueue(() => new JsonObject { ["size"] = 20 },
        _ => Task.FromResult(new JsonObject { ["ok"] = accept, ["error"] = "synthetic_failure" }),
        _ => { }, _ => throw new InvalidOperationException("closed error surface"));
    queue.Edited();
    Check(!await queue.SaveAsync().WaitAsync(TimeSpan.FromSeconds(1)), "failure must settle even when its observer throws");
    Check(queue.Dirty, "failed edit lost its retry snapshot");
    accept = true;
    Check(await queue.SaveAsync() && !queue.Dirty, "retry remained wedged after observer failure");
}

static Task SurfaceRefreshRouting()
{
    foreach (var path in new[] { "appearance.bar", "appearance.bar.shadow", "dictation.fontSize", "dictation.showText", "appearance.voiceScale", "appearance.ribbonScale", "appearance.voiceTextGap", "dictation" })
        Check(SettingsRefresh.For(path) == new SettingsRefresh(true, false, false, false), "bar routing: " + path);
    foreach (var path in new[] { "captions", "captions.fontSize", "captions.widthPercent", "appearance.captions", "appearance.captions.text" })
        Check(SettingsRefresh.For(path) == new SettingsRefresh(false, true, false, false), "captions routing: " + path);
    foreach (var path in new[] { "appearance.cards", "appearance.cards.padding" })
        Check(SettingsRefresh.For(path) == new SettingsRefresh(false, false, true, false), "cards routing: " + path);
    foreach (var path in new[] { "*", "appearance", "appearance.theme" })
        Check(SettingsRefresh.For(path) == new SettingsRefresh(true, true, true, true), "full reset: " + path);
    foreach (var path in new[] { "appearance.styleSurface", "appearance.styleCard", "hotkeys.dictation", "services", "general.barPosition", "appearance.cardsExtra" })
        Check(SettingsRefresh.For(path) == default, "unrelated setting must not relayout surfaces: " + path);
    return Task.CompletedTask;
}

static async Task ClipboardRetry()
{
    var attempts = new List<string>();
    var clipboard = new UnwrittenClipboard(text => { attempts.Add(text); return attempts.Count >= 2; }, _ => Task.CompletedTask);
    await clipboard.UpdateAsync("remaining suffix");
    Check(clipboard.Copied && !clipboard.Failed && attempts.SequenceEqual(new[] { "remaining suffix", "remaining suffix" }), "busy clipboard did not recover");
    await clipboard.UpdateAsync("remaining suffix", retry: true);
    Check(attempts.Count == 2, "unchanged final result overwrote clipboard again");
    await clipboard.UpdateAsync("final corrected suffix");
    Check(clipboard.Copied && attempts.Last() == "final corrected suffix", "final revision was not copied");
    await clipboard.UpdateAsync("");
    Check(!clipboard.HasText && !clipboard.Copied && attempts.Count == 3, "empty output must not clear system clipboard");
}

static async Task ClipboardLatest()
{
    var wait = Signal<int>();
    var attempts = new List<string>();
    var clipboard = new UnwrittenClipboard(text => { attempts.Add(text); return text == "new suffix"; }, _ => wait.Task);
    var old = clipboard.UpdateAsync("old partial");
    Check(!clipboard.Copied, "pending text claimed copied");
    await clipboard.UpdateAsync("new suffix");
    wait.SetResult(0);
    await old;
    Check(attempts.SequenceEqual(new[] { "old partial", "new suffix" }) && clipboard.Copied, "old retry overwrote newer result");
}

static async Task ClipboardCancelAndFailure()
{
    var wait = Signal<int>();
    var attempts = 0;
    var clipboard = new UnwrittenClipboard(_ => { attempts++; return false; }, _ => wait.Task);
    var old = clipboard.UpdateAsync("cancelled text");
    clipboard.Clear();
    wait.SetResult(0);
    await old;
    Check(attempts == 1 && !clipboard.HasText && !clipboard.Failed, "clear did not cancel an old retry");
    clipboard = new UnwrittenClipboard(_ => throw new InvalidOperationException("synthetic"), _ => Task.CompletedTask);
    await clipboard.UpdateAsync("retained suffix");
    Check(clipboard.HasText && clipboard.Failed && !clipboard.Copied, "failed clipboard falsely reported copied");
}

static Task VoiceWheel()
{
    var scale = VoiceSizing.Wheel(1, 30);
    Check(scale == 1.0125, "fine delta must affect size immediately");
    scale = VoiceSizing.Wheel(scale, 90);
    Check(scale == 1.05, "one total notch enlarges by 5 percent");
    Check(VoiceSizing.Wheel(scale, -120) == 1, "reverse notch restores original size");
    Check(VoiceSizing.Wheel(1, 12000) == 3 && VoiceSizing.Wheel(1, -12000) == 1, "orb bounds hold");
    Check(VoiceSizing.Wheel(1.5, 12000, true) == 3 && VoiceSizing.Wheel(1.5, -12000, true) == 1, "ribbon bounds hold");
    Check(VoiceSizing.Clamp(double.NaN) == 1.5 && VoiceSizing.Clamp(double.PositiveInfinity, true) == 1.5, "invalid persisted scale is safe");
    var first = VoiceSizing.Advance(1, 1.2, .02);
    var second = VoiceSizing.Advance(first, 1.2, .02);
    Check(Math.Abs(first-1.01)<1e-9 && Math.Abs(second-1.02)<1e-9, "equal time gives equal scale increments");
    Check(VoiceSizing.Advance(1.19,1.2,1)==1.2 && VoiceSizing.Advance(1.01,1,1)==1, "transition cannot overshoot");
    Check(Math.Abs(VoiceSizing.Advance(second,1,.02)-first)<1e-9, "retarget reverses from current size without jumping");
    return Task.CompletedTask;
}

static Task VoiceLevels()
{
    var voice = new VoiceMotion();
    voice.SetVisible(true);
    voice.OnLevel(1, 0);
    voice.Step(0.1, 0.1);
    Check(voice.Level == 0, "idle must not invent audio input");
    voice.SetPhase("listening");
    voice.OnLevel(0.5, 0.1);
    voice.Step(0.1, 0.2);
    Check(voice.Level is > 0.5 and < 0.8, "real level must drive a bounded envelope");
    for (var i = 0; i < 15; i++) voice.Step(0.1, 0.4 + i * 0.1);
    Check(voice.Level == 0, "missing input must decay to silence");
    voice.OnLevel(double.NaN, 2);
    voice.Step(0.1, 2.1);
    Check(voice.Level == 0, "invalid levels must not poison rendering");
    voice.OnLevel(10, 2.2);
    voice.Step(0.1, 2.3);
    Check(voice.Level is > 0 and <= 1, "levels must be clamped");
    voice.SetPhase("finishing");
    Check(voice.Level == 0 && voice.Processing, "finishing must clear microphone amplitude");
    return Task.CompletedTask;
}

static Task VoiceVisibility()
{
    var voice = new VoiceMotion();
    voice.SetPhase("listening");
    voice.SetVisible(true);
    voice.OnLevel(1, 0);
    voice.Step(0.1, 0.1);
    voice.SetVisible(false);
    var time = voice.Time;
    voice.OnLevel(1, 0.2);
    voice.Step(0.1, 0.3);
    Check(!voice.Running && voice.Time == time && voice.Level == 0, "hidden visuals must freeze and forget input");
    voice.SetVisible(true);
    voice.Step(0.1, 0.4);
    Check(voice.Level == 0, "showing must not replay hidden audio");
    voice.Animations = false;
    time = voice.Time;
    voice.OnLevel(1, 0.5);
    voice.Step(0.1, 0.6);
    Check(!voice.Running && voice.Time == time, "Windows reduced motion must stop animation");
    return Task.CompletedTask;
}
static JsonObject Settings(int value) => new() { ["value"] = value };
static JsonObject Success(int value) => new() { ["ok"] = true, ["settings"] = Settings(value) };
static JsonObject Failure() => new() { ["ok"] = false, ["error"] = "disk_full" };

static async Task QueuedCallIsDrained()
{
    var calls = new PendingCalls();
    var scheduled = Signal<int>();
    var accepted = calls.Run(() => scheduled.Task)!;
    var drained = calls.StopAsync();
    Check(!drained.IsCompleted, "accepted work was ignored before it started");
    scheduled.SetResult(42);
    Check(await accepted == 42, "accepted work must run while draining");
    await drained;
}

static async Task ShutdownRejectsNewCalls()
{
    var calls = new PendingCalls();
    await calls.StopAsync();
    var ran = false;
    Check(calls.Run(() => { ran = true; return Task.FromResult(1); }) is null, "new work accepted");
    Check(!ran, "rejected work executed");
}

static async Task FailedCallReleases()
{
    var calls = new PendingCalls();
    var pending = Signal<int>();
    var accepted = calls.Run(() => pending.Task)!;
    var drained = calls.StopAsync();
    pending.SetException(new IOException("synthetic failure"));
    try { await accepted; throw new InvalidOperationException("failure disappeared"); }
    catch (IOException) { }
    await drained;
}

static async Task FailedShutdownCanResume()
{
    var calls = new PendingCalls();
    var first = Signal<int>();
    var accepted = calls.Run(() => first.Task)!;
    var stopped = calls.StopAsync();
    Check(!calls.Resume(), "calls resumed before the accepted work drained");
    first.SetResult(1);
    await accepted;
    await stopped;

    // A failed persistence reply keeps the core alive and resumes normal admission.
    Check(calls.Resume() && calls.Accepting, "failed shutdown could not resume admission");
    var retryWrite = Signal<int>();
    var resumed = calls.Run(() => retryWrite.Task);
    Check(resumed is not null, "work was rejected after shutdown failed");
    var retriedStop = calls.StopAsync();
    Check(!retriedStop.IsCompleted, "retry reused the earlier completed drain");
    Check(calls.Run(() => Task.FromResult(3)) is null, "retry accepted new work while stopping");
    retryWrite.SetResult(2);
    Check(await resumed! == 2, "resumed write was lost on the next shutdown");
    await retriedStop;
    Check(!calls.Accepting, "successful shutdown resumed admission");
}

static async Task SettingsCoalesce()
{
    var current = Settings(1);
    var first = Signal<JsonObject>();
    var second = Signal<JsonObject>();
    var secondStarted = Signal<int>();
    var sent = new List<int>();
    var applied = new List<int>();
    var queue = new SettingsSaveQueue(() => (JsonObject)current.DeepClone(), value =>
    {
        sent.Add(value["value"]!.GetValue<int>());
        if (sent.Count == 1) return first.Task;
        secondStarted.SetResult(sent.Count);
        return second.Task;
    }, reply =>
    {
        current = (JsonObject)reply["settings"]!.DeepClone();
        applied.Add(current["value"]!.GetValue<int>());
    }, _ => throw new InvalidOperationException("unexpected failure"));
    queue.Edited();
    var save = queue.SaveAsync();
    current = Settings(2); queue.Edited();
    current = Settings(3); queue.Edited();
    Check(ReferenceEquals(save, queue.SaveAsync()), "parallel settings writes started");
    first.SetResult(Success(1));
    await secondStarted.Task;
    Check(applied.Count == 0 && current["value"]!.GetValue<int>() == 3, "old reply overwrote an edit");
    Check(sent.SequenceEqual(new[] { 1, 3 }), "pending edits were not coalesced");
    second.SetResult(Success(3));
    Check(await save && !queue.Dirty, "latest snapshot was not committed");
    Check(applied.SequenceEqual(new[] { 3 }), "wrong snapshot applied");
}

static async Task FailedSettingsRetry()
{
    var attempts = 0;
    var errors = 0;
    var queue = new SettingsSaveQueue(() => Settings(5), _ =>
        Task.FromResult(++attempts == 1 ? Failure() : Success(5)), _ => { }, _ => errors++);
    queue.Edited();
    Check(!await queue.SaveAsync() && queue.Dirty && errors == 1, "failed edit was lost");
    Check(await queue.SaveAsync() && !queue.Dirty && attempts == 2, "retry did not save");
}

static async Task NewerSnapshotAfterFailure()
{
    var current = Settings(1);
    var first = Signal<JsonObject>();
    var attempts = 0;
    var queue = new SettingsSaveQueue(() => (JsonObject)current.DeepClone(), _ =>
        ++attempts == 1 ? first.Task : Task.FromResult(Success(2)), _ => { }, _ => { });
    queue.Edited();
    var save = queue.SaveAsync();
    current = Settings(2); queue.Edited();
    first.SetResult(Failure());
    Check(await save && attempts == 2, "new edit was stranded behind a failed write");
}

static async Task SettingsExceptionRecovery()
{
    var attempts = 0;
    var queue = new SettingsSaveQueue(() => Settings(1), _ =>
        ++attempts == 1 ? throw new IOException("synthetic") : Task.FromResult(Success(1)), _ => { }, _ => { });
    queue.Edited();
    Check(!await queue.SaveAsync() && queue.Dirty, "exception lost dirty state");
    Check(await queue.SaveAsync(), "exception left the save loop stuck");
}
