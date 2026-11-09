using System.ComponentModel;
using System.Diagnostics;

namespace LiveListen.Services;

/// <summary>Hands the verified package to setup's in-place upgrade flow.</summary>
internal static class UpdateInstaller
{
    public static async Task RunAsync(string package, string installDirectory, CancellationToken token)
    {
        token.ThrowIfCancellationRequested();
        var folder = Path.GetDirectoryName(package)!;
        var start = new ProcessStartInfo(package)
        {
            UseShellExecute = false,
            WorkingDirectory = folder,
            ArgumentList =
            {
                "/UPDATE",
                "/DIR=" + Path.TrimEndingDirectorySeparator(installDirectory),
                "/LOG=" + Path.Combine(folder, "update-install.log"),
            },
        };
        try
        {
            // Setup requests a normal exit and waits for pending saves before swapping files.
            using var process = Process.Start(start) ?? throw new UpdateException("install");
            await process.WaitForExitAsync(token).ConfigureAwait(false);
            if (process.ExitCode != 0) throw new UpdateException("install");
        }
        catch (Win32Exception)
        {
            throw new UpdateException("install");
        }
    }
}
