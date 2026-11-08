using System.Text.Json.Nodes;
using LiveListen.Controls;
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
    private readonly List<NoteEditor> cardEditors = [];
    private readonly List<Func<Task<bool>>> cardFlushers = [];
    private readonly HashSet<NoteEditor> dirtyEditors = [];
    private Task<bool>? flushTask;
    private bool flushing;
    private bool newCardFlyoutOpen;

    public NotebookView(App app, Window owner)
    {
        this.app = app;
        this.owner = owner;
        owner.Activated += OnOwnerActivated;
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

    public void Release()
    {
        closed = true;
        pageEvents = null;
        owner.Activated -= OnOwnerActivated;
    }

    private void OnOwnerActivated(object sender, WindowActivatedEventArgs e)
    {
        if (e.WindowActivationState == WindowActivationState.Deactivated && !cardEditors.Any(editor => editor.HasOpenFlyout))
            FinishEditing();
    }

    public void OnEngineEvent(string name, JsonObject e) => pageEvents?.Invoke(name, e);

    public bool FinishEditing()
    {
        foreach (var editor in cardEditors.ToArray()) editor.FinishEditing();
        return cardEditors.All(editor => editor.CanSave);
    }

    /// <summary>Keeps row drafts alive until the engine has accepted every queued patch.</summary>
    public Task<bool> FlushAsync()
    {
        if (flushTask is not null) return flushTask;
        var completion = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        flushTask = completion.Task;
        _ = FlushRowsAsync(completion);
        return completion.Task;
    }

    private async Task FlushRowsAsync(TaskCompletionSource<bool> completion)
    {
        flushing = true;
        root.IsHitTestVisible = false;
        var pausedEditors = cardEditors.ToArray();
        var accepted = false;
        try
        {
            // Hit testing only blocks the pointer; the focused RichEditBox can still
            // receive typing while a save is awaiting the engine unless it is paused.
            foreach (var editor in pausedEditors) editor.Pause();
            if (FinishEditing())
            {
                var results = await Task.WhenAll(cardFlushers.ToArray().Select(flush => flush()));
                accepted = results.All(result => result) && dirtyEditors.Count == 0;
            }
            if (!accepted)
            {
                cardsStatus.Text = Loc.Error(cardEditors.Any(editor => !editor.CanSave) ? "bad_rich_text" : "save_failed");
            }
        }
        catch (Exception) { cardsStatus.Text = Loc.Error("save_failed"); }
        finally
        {
            foreach (var editor in pausedEditors) editor.Resume();
            flushing = false;
            root.IsHitTestVisible = true;
            flushTask = null;
            completion.TrySetResult(accepted);
            QueueCardsRefresh();
        }
    }

    private void QueueCardsRefresh() => owner.DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
    {
        if (!closed && cardsPending && !Editing()) FillCards();
    });

    // ---- building blocks ---------------------------------------------------------------

    private static string T(string key) => Loc.T(key);

    private static T Res<T>(string key) => (T)Application.Current.Resources[key];

    /// <summary>Keep editing and new-card flyouts stable until they close.</summary>
    private bool Editing()
    {
        if (flushing || newCardFlyoutOpen || dirtyEditors.Count > 0 || cardEditors.Any(editor => editor.IsEditing || editor.HasOpenFlyout)) return true;
        if (root.XamlRoot is null || FocusManager.GetFocusedElement(root.XamlRoot) is not TextBox text) return false;
        // Text boxes on other settings pages must not hold up notebook refreshes.
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
