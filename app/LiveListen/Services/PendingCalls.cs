namespace LiveListen.Services;

/// <summary>Counts calls when accepted, including work still waiting for a thread-pool thread.</summary>
internal sealed class PendingCalls
{
    private readonly object gate = new();
    private TaskCompletionSource drained = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private bool accepting = true;
    private int count;

    public bool Accepting { get { lock (gate) { return accepting; } } }

    public Task<T>? Run<T>(Func<Task<T>> work)
    {
        lock (gate)
        {
            if (!accepting)
            {
                return null;
            }
            count++;
        }
        return Finish(work);
    }

    private async Task<T> Finish<T>(Func<Task<T>> work)
    {
        try
        {
            return await work().ConfigureAwait(false);
        }
        finally
        {
            lock (gate)
            {
                count--;
                if (!accepting && count == 0)
                {
                    drained.TrySetResult();
                }
            }
        }
    }

    public Task StopAsync()
    {
        lock (gate)
        {
            accepting = false;
            if (count == 0)
            {
                drained.TrySetResult();
            }
            return drained.Task;
        }
    }

    /// <summary>Reopens admission after a failed shutdown, once accepted work has drained.</summary>
    public bool Resume()
    {
        lock (gate)
        {
            if (accepting || count != 0 || !drained.Task.IsCompleted)
            {
                return false;
            }
            drained = new(TaskCreationOptions.RunContinuationsAsynchronously);
            accepting = true;
            return true;
        }
    }
}
