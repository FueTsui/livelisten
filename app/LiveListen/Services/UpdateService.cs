using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace LiveListen.Services;

internal static class UpdateService
{
    private static readonly HashSet<Window> pending = [];
    private static readonly Uri Releases = new("https://github.com/FueTsui/livelisten/releases/latest");

    public static async Task CheckAsync(Window owner)
    {
        if (owner.Content is not FrameworkElement { XamlRoot: { } root } content || !pending.Add(owner)) return;
        var dialog = new ContentDialog
        {
            XamlRoot = root,
            RequestedTheme = content.ActualTheme,
            Title = Loc.T("gen.update"),
            Content = new HyperlinkButton
            {
                Content = Loc.T("gen.update.releases"),
                NavigateUri = Releases,
            },
            CloseButtonText = Loc.T("gen.update.close"),
        };
        void Closed(object sender, WindowEventArgs args) => dialog.Hide();
        owner.Closed += Closed;
        try { await dialog.ShowAsync(); }
        finally { owner.Closed -= Closed; pending.Remove(owner); }
    }
}
