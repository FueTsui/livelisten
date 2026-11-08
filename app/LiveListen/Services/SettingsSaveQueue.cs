using System.Text.Json.Nodes;

namespace LiveListen.Services;

/// <summary>One settings write at a time. Edits made while saving replace the next snapshot.</summary>
/// <remarks>Called on the UI thread; awaits resume on that thread.</remarks>
internal sealed class SettingsSaveQueue(
    Func<JsonObject> snapshot,
    Func<JsonObject, Task<JsonObject>> send,
    Action<JsonObject> applied,
    Action<string> failed)
{
    private long version;
    private long savedVersion;
    private TaskCompletionSource<bool>? running;

    public bool Dirty => version != savedVersion;

    public void Edited() => version++;

    public Task<bool> SaveAsync()
    {
        if (running is not null)
        {
            return running.Task;
        }
        if (!Dirty)
        {
            return Task.FromResult(true);
        }
        var completion = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        running = completion;
        _ = Run(completion);
        return completion.Task;
    }

    private async Task Run(TaskCompletionSource<bool> completion)
    {
        try
        {
            while (Dirty)
            {
                var sentVersion = version;
                var reply = await send(snapshot());
                if (reply["ok"]?.GetValue<bool>() != true)
                {
                    ReportFailure(reply["error"]?.GetValue<string>() ?? "save_failed");
                    // A newer edit still deserves its own attempt; do not spin on a failed snapshot.
                    if (sentVersion == version)
                    {
                        break;
                    }
                    continue;
                }
                savedVersion = sentVersion;
                if (sentVersion == version)
                {
                    applied(reply);
                }
            }
            completion.TrySetResult(!Dirty);
        }
        catch (Exception)
        {
            ReportFailure("save_failed");
            completion.TrySetResult(false);
        }
        finally
        {
            running = null;
        }
    }

    private void ReportFailure(string code)
    {
        try
        {
            failed(code);
        }
        catch (Exception)
        {
            // A closed/broken error surface must not leave SaveAsync permanently
            // pending. The dirty snapshot remains available for the next retry.
        }
    }
}
