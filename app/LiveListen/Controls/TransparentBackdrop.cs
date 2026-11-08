using System.Runtime.InteropServices;
using Microsoft.UI.Composition;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;

namespace LiveListen.Controls;

/// <summary>
/// A system backdrop made of a fully transparent brush, so only the drawn surface of an
/// overlay window is visible.
/// </summary>
internal sealed partial class TransparentBackdrop : SystemBackdrop
{
    private static Windows.UI.Composition.Compositor? compositor;
    private static nint queueController;

    protected override void OnTargetConnected(ICompositionSupportsSystemBackdrop target, XamlRoot root)
    {
        base.OnTargetConnected(target, root);
        target.SystemBackdrop = Compositor.CreateColorBrush(Windows.UI.Color.FromArgb(0, 0, 0, 0));
    }

    protected override void OnTargetDisconnected(ICompositionSupportsSystemBackdrop target)
    {
        base.OnTargetDisconnected(target);
        target.SystemBackdrop = null;
    }

    // The base class answers theme and high-contrast changes by applying the default backdrop,
    // which throws "target" for a custom brush. The transparent brush needs no update.
    protected override void OnDefaultSystemBackdropConfigurationChanged(ICompositionSupportsSystemBackdrop target, XamlRoot xamlRoot)
    {
    }

    // The system backdrop needs a Windows.UI.Composition brush, which needs a
    // Windows.System.DispatcherQueue on this thread.
    private static Windows.UI.Composition.Compositor Compositor
    {
        get
        {
            if (compositor is null)
            {
                if (Windows.System.DispatcherQueue.GetForCurrentThread() is null)
                {
                    var options = new DispatcherQueueOptions
                    {
                        Size = Marshal.SizeOf<DispatcherQueueOptions>(),
                        ThreadType = 2, // DQTYPE_THREAD_CURRENT
                        ApartmentType = 0, // DQTAT_COM_NONE
                    };
                    _ = CreateDispatcherQueueController(options, out queueController);
                }
                compositor = new Windows.UI.Composition.Compositor();
            }
            return compositor;
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct DispatcherQueueOptions
    {
        public int Size;
        public int ThreadType;
        public int ApartmentType;
    }

    [DllImport("CoreMessaging.dll")]
    private static extern int CreateDispatcherQueueController(DispatcherQueueOptions options, out nint controller);
}
