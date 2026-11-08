using System.Text.Json.Nodes;
using LiveListen.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;

namespace LiveListen.Views;

/// <summary>The spoken-notes settings page: the list of note and to-do cards.</summary>
internal sealed partial class NotebookView
{
    private readonly App app;
    private readonly Window owner;
    private readonly Grid root = new();
    private readonly Grid pageHost = new();

    private Action<string, JsonObject>? pageEvents;
    private bool closed;

    public NotebookView(App app, Window owner)
    {
        this.app = app;
        this.owner = owner;
        owner.Closed += (_, _) => { closed = true; pageEvents = null; };
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        root.Children.Add(new TextBlock
        {
            Text = T("nav.notebook"),
            Style = Res<Style>("TitleTextBlockStyle"),
            Margin = new Thickness(36, 12, 36, 8),
        });
        Grid.SetRow(pageHost, 1);
        root.Children.Add(pageHost);
        BuildCards();
    }

    public FrameworkElement View => root;

    public void OnEngineEvent(string name, JsonObject e) => pageEvents?.Invoke(name, e);

    // ---- building blocks ---------------------------------------------------------------

    private static string T(string key) => Loc.T(key);

    private static T Res<T>(string key) => (T)Application.Current.Resources[key];

    /// <summary>Only an existing card editor delays replacing list rows.</summary>
    private bool Editing()
    {
        if (root.XamlRoot is null || FocusManager.GetFocusedElement(root.XamlRoot) is not TextBox text) return false;
        // New-note flyouts and text boxes on other settings pages do not belong to
        // the rows being rebuilt; treating them as row edits left cardsPending stuck.
        for (DependencyObject? node = text; node is not null; node = VisualTreeHelper.GetParent(node))
            if (ReferenceEquals(node, cardsPanel)) return true;
        return false;
    }

    private static TextBlock Secondary(string text = "") => new()
    {
        Text = text,
        Style = Res<Style>("SecondaryTextStyle"),
    };

    private static StackPanel Horizontal(params UIElement[] children)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        foreach (var child in children)
        {
            row.Children.Add(child);
        }
        return row;
    }
}
