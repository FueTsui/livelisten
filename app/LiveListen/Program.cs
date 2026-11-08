using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.Windows.AppLifecycle;

namespace LiveListen;

public static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        WinRT.ComWrappersSupport.InitializeComWrappers();

        // One instance only: a second launch asks the running one to show its bar. A test run
        // with its own settings folder is a separate instance and never reaches the user's app.
        var key = AppInstance.FindOrRegisterForKey(Services.EngineHost.TestConfigDir is { } dir
            ? "LiveListen.Test." + Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(
                System.Text.Encoding.UTF8.GetBytes(dir.ToUpperInvariant())))[..16]
            : "LiveListen.Main");
        // `--quit` asks a running instance to exit normally (the installer uses it before it
        // replaces files); with no instance running there is nothing to do.
        var quit = args.Any(a => a.Equals("--quit", StringComparison.OrdinalIgnoreCase));
        if (key.IsCurrent && quit)
        {
            return 0;
        }
        if (!key.IsCurrent)
        {
            var activation = AppInstance.GetCurrent().GetActivatedEventArgs();
            // This launch came from the user, so it may hand the foreground to the running
            // instance; otherwise `--settings` would open behind the current window.
            Interop.Win32.AllowSetForegroundWindow(key.ProcessId);
            Task.Run(() => key.RedirectActivationToAsync(activation).AsTask()).Wait(TimeSpan.FromSeconds(5));
            return 0;
        }
        key.Activated += (_, e) => App.Instance?.OnRelaunched(
            (e.Data as Windows.ApplicationModel.Activation.ILaunchActivatedEventArgs)?.Arguments ?? "");

        Application.Start(p =>
        {
            var context = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
            SynchronizationContext.SetSynchronizationContext(context);
            _ = new App(args);
        });
        return 0;
    }
}
