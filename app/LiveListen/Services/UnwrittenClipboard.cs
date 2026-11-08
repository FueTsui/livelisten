namespace LiveListen.Services;

/// <summary>Copies only the current unwritten suffix. A newer result cancels stale retries.</summary>
internal sealed class UnwrittenClipboard(Func<string, bool> write, Func<int, Task>? delay = null)
{
    private readonly Func<int, Task> wait = delay ?? Task.Delay;
    private string pending = "";
    private string copied = "";
    private int version;
    public event Action? Changed;
    public bool HasText => pending.Length > 0;
    public bool Copied => HasText && pending == copied;
    public bool Failed { get; private set; }

    public void Clear()
    {
        version++;
        pending = copied = "";
        Failed = false;
        Changed?.Invoke();
    }

    public async Task UpdateAsync(string text, bool retry = false)
    {
        if (text.Length == 0) { if (HasText) Clear(); return; }
        if (text == pending && (!retry || !Failed)) return;
        pending = text;
        copied = "";
        Failed = false;
        var request = ++version;
        Changed?.Invoke();
        for (var attempt = 0; attempt < 5; attempt++)
        {
            if (attempt > 0) await wait(50 << (attempt - 1));
            if (request != version) return;
            bool success;
            try { success = write(text); }
            catch { success = false; }
            if (success)
            {
                copied = text;
                Changed?.Invoke();
                return;
            }
        }
        Failed = true;
        Changed?.Invoke();
    }
}
