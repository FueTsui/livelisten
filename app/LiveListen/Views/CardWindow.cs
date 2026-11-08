using System.Text.Json.Nodes;
using LiveListen.Controls;
using LiveListen.Services;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;

namespace LiveListen.Views;

/// <summary>
/// One note or to-do card on the desktop. It sits with other windows (not on top) and takes
/// focus when clicked, so its text can be edited in place.
/// </summary>
internal sealed partial class CardWindow : Window
{
    private const double CardWidth = 264;

    private readonly App app;
    private readonly DispatcherQueue dispatcher;
    private readonly Surface surface;
    private readonly Grid root = new() { Background = new SolidColorBrush(Palette.Transparent) };
    private readonly Grid frame = new() { Width = CardWidth, VerticalAlignment = VerticalAlignment.Top };
    private readonly Grid surfaceHost = new();
    private readonly StackPanel body = new() { Spacing = 4 };
    private readonly FontIcon kindIcon = new() { Glyph = "", FontSize = 14, VerticalAlignment = VerticalAlignment.Center };
    private readonly CheckBox doneBox = new() { MinWidth = 0, Padding = new Thickness(0), VerticalAlignment = VerticalAlignment.Center };
    private readonly TextBlock date = new() { FontSize = 12, VerticalAlignment = VerticalAlignment.Center };
    private readonly Grid handle = new() { Background = new SolidColorBrush(Palette.Transparent), MinHeight = 28 };
    private readonly GlyphButton archive = new("", Loc.T("card.archive"), size: 26, iconSize: 12);
    private readonly GlyphButton hide = new("", Loc.T("card.hide"), size: 26, iconSize: 10);
    private readonly TextBox editor = new()
    {
        AcceptsReturn = true,
        TextWrapping = TextWrapping.Wrap,
        BorderThickness = new Thickness(0),
        Padding = new Thickness(0, 2, 0, 2),
        MinHeight = 0,
        MaxHeight = 320,
        FontSize = 14,
    };

    // Brushes the text box template reads; their colors follow the card style.
    private readonly SolidColorBrush clear = new(Palette.Transparent);
    private readonly SolidColorBrush textBrush = new();
    private readonly SolidColorBrush secondaryBrush = new();

    private SurfaceLook look;
    private JsonObject card;
    // A style shown while it is edited in the settings, before it is saved.
    private JsonObject? previewStyle;
    private bool syncing;
    private bool fitQueued;
    private bool closing;
    private Task saves = Task.CompletedTask;
    private long patchVersion;
    private readonly Dictionary<string, (long Version, JsonNode? Value)> pendingFields = [];

    public CardWindow(App app, JsonObject card)
    {
        this.app = app;
        this.card = card;
        dispatcher = DispatcherQueue;
        Id = (ulong)card["id"].Num();
        Title = Loc.T("card.title");

        foreach (var key in new[] { "TextControlBackground", "TextControlBackgroundPointerOver", "TextControlBackgroundFocused",
                     "TextControlBorderBrush", "TextControlBorderBrushPointerOver", "TextControlBorderBrushFocused" })
        {
            editor.Resources[key] = clear;
        }
        foreach (var key in new[] { "TextControlForeground", "TextControlForegroundPointerOver", "TextControlForegroundFocused" })
        {
            editor.Resources[key] = textBrush;
        }
        editor.Resources["TextControlPlaceholderForeground"] = secondaryBrush;
        editor.Resources["TextControlPlaceholderForegroundFocused"] = secondaryBrush;
        editor.Resources["TextControlBorderThemeThicknessFocused"] = new Thickness(0);
        editor.PlaceholderText = Loc.T("card.placeholder");
        AutomationProperties.SetName(editor, Loc.T("card.text"));
        editor.TextChanged += (_, _) => QueueFit();
        editor.LostFocus += (_, _) => SaveText();

        var header = new Grid { ColumnSpacing = 6 };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var lead = new Grid { VerticalAlignment = VerticalAlignment.Center };
        lead.Children.Add(kindIcon);
        lead.Children.Add(doneBox);
        AutomationProperties.SetName(doneBox, Loc.T("card.done"));
        doneBox.Checked += (_, _) => Patch(new JsonObject { ["done"] = true });
        doneBox.Unchecked += (_, _) => Patch(new JsonObject { ["done"] = false });
        handle.Children.Add(date);
        AutomationProperties.SetName(handle, Loc.T("bar.drag"));
        Grid.SetColumn(handle, 1);
        Grid.SetColumn(archive.View, 2);
        Grid.SetColumn(hide.View, 3);
        header.Children.Add(lead);
        header.Children.Add(handle);
        header.Children.Add(archive.View);
        header.Children.Add(hide.View);
        ToolTipService.SetToolTip(archive.View, Loc.T("card.archive"));
        ToolTipService.SetToolTip(hide.View, Loc.T("card.hide"));
        archive.Click += () => Patch(new JsonObject { ["archived"] = true });
        hide.Click += () => Patch(new JsonObject { ["visible"] = false });
        _ = new WindowDrag(this, handle, SavePosition) { Snap = SnapTo };
        var when = WhenPicker.Create(() => (long)card["createdMs"].Num(), () => root.ActualTheme,
            ms => Patch(new JsonObject { ["createdMs"] = ms }));
        handle.DoubleTapped += (_, e) =>
        {
            e.Handled = true;
            when.ShowAt(date);
        };
        ToolTipService.SetToolTip(handle, Loc.T("card.date.tip"));
        frame.ContextFlyout = CardMenu(when);

        body.Children.Add(header);
        body.Children.Add(editor);
        frame.Children.Add(surfaceHost);
        frame.Children.Add(body);
        root.Children.Add(frame);
        Content = root;
        surface = new Surface(surfaceHost);
        Overlay.Prepare(this, floating: false);
        AppWindow.Closing += (_, e) =>
        {
            if (!app.Exiting && !closing)
            {
                // Alt+F4 hides the card from the desktop; it stays in the list.
                e.Cancel = true;
                Patch(new JsonObject { ["visible"] = false });
            }
        };
        var scale = Overlay.Scale(this);
        AppWindow.Resize(new SizeInt32((int)((CardWidth + 32) * scale), (int)(120 * scale)));
        Refresh();
        Update(card);
        root.Loaded += (_, _) => QueueFit();
        Closed += (_, _) => closing = true;
    }

    public ulong Id { get; }

    /// <summary>The drawn card in screen pixels, without the room for its shadow.</summary>
    public RectInt32 CardRect => Inset(AppWindow.Position);

    private RectInt32 Inset(PointInt32 p)
    {
        var m = (int)Math.Round(look.ShadowMargin * Overlay.Scale(this));
        var size = AppWindow.Size;
        return new RectInt32(p.X + m, p.Y + m, Math.Max(1, size.Width - 2 * m), Math.Max(1, size.Height - 2 * m));
    }

    /// <summary>Snaps a dragged card to screen edges and to the other cards.</summary>
    private PointInt32 SnapTo(PointInt32 p)
    {
        var rect = Inset(p);
        var center = new PointInt32(rect.X + rect.Width / 2, rect.Y + rect.Height / 2);
        var work = DisplayArea.GetFromPoint(center, DisplayAreaFallback.Nearest).WorkArea;
        var move = CardSnap.Offset(rect, work, app.Cards.CardRects(Id), Overlay.Scale(this), look.ShadowMargin);
        return new PointInt32(p.X + move.X, p.Y + move.Y);
    }

    /// <summary>Places the card (its saved spot, or a free spot near the top right) and shows it.</summary>
    public void ShowCard(int index)
    {
        Fit();
        if (card["x"] is JsonNode x && card["y"] is JsonNode y)
        {
            Overlay.PlaceAt(this, (int)x.Num(), (int)y.Num());
        }
        else
        {
            var work = DisplayArea.Primary.WorkArea;
            var scale = Overlay.Scale(this);
            var size = AppWindow.Size;
            var step = (int)(36 * scale);
            var px = work.X + work.Width - size.Width - (int)(24 * scale) - index % 6 * step / 2;
            var py = work.Y + (int)(24 * scale) + index % 6 * step;
            AppWindow.Move(new PointInt32(px, py));
            SavePosition(AppWindow.Position);
        }
        Overlay.ShowQuiet(this);
        QueueFit();
    }

    public void CloseCard()
    {
        closing = true;
        Close();
    }

    /// <summary>Commits the focused editor and drains patches before the engine can stop.</summary>
    public async Task<bool> FlushAsync()
    {
        SaveText();
        PauseEditing();
        await saves;
        if (pendingFields.Count > 0)
        {
            Patch(new JsonObject());
            await saves;
        }
        return pendingFields.Count == 0;
    }

    public void PauseEditing()
    {
        editor.IsReadOnly = true;
        root.IsHitTestVisible = false;
    }

    public void ResumeEditing()
    {
        editor.IsReadOnly = false;
        root.IsHitTestVisible = true;
    }

    public void Update(JsonObject value)
    {
        var oldStyle = card["style"];
        card = (JsonObject)value.DeepClone();
        // An earlier cards.changed event must not flash old values over local edits
        // whose queued patch has not reached the engine yet.
        foreach (var (key, pending) in pendingFields)
        {
            card[key] = pending.Value?.DeepClone();
        }
        if (!JsonNode.DeepEquals(oldStyle, card["style"]))
        {
            Refresh();
        }
        syncing = true;
        var todo = card["kind"].Str() == "todo";
        var done = card["done"].Bool();
        kindIcon.Visibility = todo ? Visibility.Collapsed : Visibility.Visible;
        doneBox.Visibility = todo ? Visibility.Visible : Visibility.Collapsed;
        doneBox.IsChecked = done;
        date.Text = Format.Date(card["created"].Str());
        if (editor.FocusState == FocusState.Unfocused)
        {
            editor.Text = card["text"].Str();
        }
        editor.Opacity = done ? 0.55 : 1;
        syncing = false;
        QueueFit();
    }

    /// <summary>True when this card has a style of its own instead of the shared one.</summary>
    public bool HasOwnStyle => app.Cards.EffectiveStyle(Id) is not null;

    /// <summary>Shows a style without saving it; <c>null</c> goes back to the saved one.</summary>
    public void PreviewStyle(JsonObject? style)
    {
        previewStyle = style?.DeepClone() as JsonObject;
        Refresh();
    }

    /// <summary>Saves the card's own style; <c>null</c> makes it follow the shared style.</summary>
    public Task SetStyle(JsonObject? style) => app.Cards.SetStyle(Id, style);

    /// <summary>Re-reads the card style: its own one, or the shared one from the settings.</summary>
    public void Refresh()
    {
        look = Theme.Look(app.Settings, "cards", previewStyle ?? app.Cards.EffectiveStyle(Id));
        root.RequestedTheme = look.Dark ? ElementTheme.Dark : ElementTheme.Light;
        frame.Margin = new Thickness(look.ShadowMargin);
        surface.Apply(look);
        body.Padding = new Thickness(Math.Max(8, look.Padding), Math.Max(4, look.Padding - 6), Math.Max(6, look.Padding - 4), Math.Max(8, look.Padding));
        textBrush.Color = look.Text;
        secondaryBrush.Color = look.Secondary;
        editor.Foreground = textBrush;
        date.Foreground = secondaryBrush;
        kindIcon.Foreground = secondaryBrush;
        archive.Apply(look);
        hide.Apply(look);
        QueueFit();
    }

    /// <summary>The right-click menu; it is rebuilt each time so it matches the card.</summary>
    private MenuFlyout CardMenu(Flyout when)
    {
        var menu = new MenuFlyout { ShouldConstrainToRootBounds = false };
        menu.Opening += (_, _) =>
        {
            menu.Items.Clear();
            var todo = card["kind"].Str() == "todo";
            void Item(string label, string glyph, Action run)
            {
                var item = new MenuFlyoutItem { Text = label, Icon = new FontIcon { Glyph = glyph } };
                // Run after the menu has closed, so a flyout it opens is not dismissed with it.
                item.Click += (_, _) => dispatcher.TryEnqueue(DispatcherQueuePriority.Low, () => run());
                menu.Items.Add(item);
            }
            Item(Loc.T(todo ? "cards.toNote" : "cards.toTodo"), todo ? "\uE70B" : "\uE73A",
                () => Patch(new JsonObject { ["kind"] = todo ? "note" : "todo" }));
            Item(Loc.T("card.menu.date"), "\uE787", () => when.ShowAt(date));
            menu.Items.Add(new MenuFlyoutSeparator());
            Item(Loc.T("card.archive"), "\uE7B8", () => Patch(new JsonObject { ["archived"] = true }));
            Item(Loc.T("card.hide"), "\uED1A", () => Patch(new JsonObject { ["visible"] = false }));
            menu.Items.Add(new MenuFlyoutSeparator());
            Item(Loc.T("card.menu.style"), "\uE790", () => app.OpenCardStyle(Id));
            if (HasOwnStyle)
            {
                Item(Loc.T("card.menu.sharedStyle"), "\uE72C", () => _ = SetStyle(null));
            }
            Item(Loc.T("card.menu.settings"), "\uE713", () => app.OpenNotebook());
        };
        return menu;
    }

    private void SaveText()
    {
        var text = editor.Text.Trim();
        if (text.Length == 0)
        {
            editor.Text = card["text"].Str();
            return;
        }
        if (text != card["text"].Str())
        {
            Patch(new JsonObject { ["text"] = text });
        }
    }

    private void Patch(JsonObject patch)
    {
        if (syncing)
        {
            return;
        }
        // Hiding or archiving can close the window before LostFocus. Include its draft.
        var draft = editor.Text.Trim();
        if (draft.Length > 0 && draft != card["text"].Str())
        {
            patch["text"] = draft;
        }
        foreach (var (key, pending) in pendingFields)
        {
            if (!patch.ContainsKey(key)) patch[key] = pending.Value?.DeepClone();
        }
        if (patch.Count == 0) return;
        var version = ++patchVersion;
        foreach (var item in patch)
        {
            card[item.Key] = item.Value?.DeepClone();
            pendingFields[item.Key] = (version, item.Value?.DeepClone());
        }
        saves = SavePatchAsync(saves, patch, version);
        app.Cards.Track(saves);
    }

    private async Task SavePatchAsync(Task previous, JsonObject patch, long version)
    {
        await previous;
        try
        {
            var reply = await app.Engine.CallAsync("cards.update", new JsonObject { ["id"] = Id, ["patch"] = patch });
            if (reply.Ok() || reply["error"].Str() == "card_save_failed")
            {
                // A disk failure still leaves the accepted edit in the core's dirty cards.
                foreach (var key in pendingFields.Where(item => item.Value.Version <= version).Select(item => item.Key).ToArray())
                    pendingFields.Remove(key);
            }
            if (!reply.Ok() && !app.Exiting)
            {
                app.Bar.ShowMessage(Loc.Error(reply["error"].Str("save_failed")), true);
            }
        }
        catch (Exception)
        {
            if (!app.Exiting)
            {
                app.Bar.ShowMessage(Loc.Error("save_failed"), true);
            }
        }
    }

    private void SavePosition(PointInt32 p) => Patch(new JsonObject { ["x"] = p.X, ["y"] = p.Y });

    private void QueueFit()
    {
        if (closing || fitQueued)
        {
            return;
        }
        fitQueued = true;
        if (!dispatcher.TryEnqueue(DispatcherQueuePriority.Low, () =>
        {
            fitQueued = false;
            if (!closing) Fit();
        })) fitQueued = false;
    }

    private void Fit() => Overlay.Fit(this, root, CardWidth + 2 * look.ShadowMargin);
}

/// <summary>Keeps one window for each card that should be on the desktop.</summary>
internal sealed class CardsHost(App app)
{
    private readonly Dictionary<ulong, CardWindow> windows = [];
    private readonly List<Task> pendingWrites = [];
    private sealed class StyleDraft(long revision, JsonObject? style)
    {
        public long Revision { get; } = revision;
        public JsonObject? Style { get; } = style;
        public bool Accepted { get; set; }
    }
    private readonly Dictionary<ulong, StyleDraft> styleDrafts = [];
    private Task styleWrites = Task.CompletedTask;
    private StyleDraft? clearDraft;
    private long styleRevision;
    private bool flushing;

    public bool Shown { get; private set; }

    public IReadOnlyList<JsonObject> Cards { get; private set; } = [];

    /// <summary>Restores the cards saved before the last exit.</summary>
    public void Load() => Apply(app.Engine.Call("cards.list"));

    /// <summary>Applies a <c>cards.list</c> reply or a <c>cards.changed</c> event.</summary>
    public void Apply(JsonObject e)
    {
        Shown = e["showOnDesktop"].Bool();
        Cards = (e["cards"] as JsonArray)?.OfType<JsonObject>().ToList() ?? [];
        RetireStyles();
        var wanted = Shown ? Cards.Where(c => c["visible"].Bool() && !c["archived"].Bool()).ToList() : [];
        var keep = wanted.Select(c => (ulong)c["id"].Num()).ToHashSet();
        foreach (var id in windows.Keys.Where(id => !keep.Contains(id)).ToList())
        {
            windows[id].CloseCard();
            windows.Remove(id);
        }
        var index = 0;
        foreach (var c in wanted)
        {
            var id = (ulong)c["id"].Num();
            if (windows.TryGetValue(id, out var window))
            {
                window.Update(c);
            }
            else
            {
                window = new CardWindow(app, c);
                windows[id] = window;
                window.ShowCard(index);
                if (flushing) window.PauseEditing();
            }
            index++;
        }
    }

    /// <summary>The latest edit, including writes queued by a settings window that closed.</summary>
    public JsonObject? EffectiveStyle(ulong id)
    {
        if (styleDrafts.TryGetValue(id, out var draft)
            && (clearDraft is null || draft.Revision > clearDraft.Revision))
        {
            return draft.Style?.DeepClone() as JsonObject;
        }
        if (clearDraft is not null) return null;
        return Cards.FirstOrDefault(card => (ulong)card["id"].Num() == id)?["style"]?.DeepClone() as JsonObject;
    }

    /// <summary>Where the other cards are drawn, for snapping.</summary>
    public List<RectInt32> CardRects(ulong except) =>
        windows.Values.Where(w => w.Id != except && Overlay.IsVisible(w)).Select(w => w.CardRect).ToList();

    public void Refresh()
    {
        foreach (var window in windows.Values)
        {
            window.Refresh();
        }
    }

    /// <summary>Shows an unsaved style on the card's window, if it is on the desktop.</summary>
    public void PreviewStyle(ulong id, JsonObject? style)
    {
        if (windows.TryGetValue(id, out var window))
        {
            window.PreviewStyle(style);
        }
    }

    /// <summary>Saves a card's own style (<c>null</c>: follow the shared style).</summary>
    public Task SetStyle(ulong id, JsonObject? style)
    {
        var draft = new StyleDraft(++styleRevision, style?.DeepClone() as JsonObject);
        styleDrafts[id] = draft;
        PreviewStyle(id, null);
        // Enqueue now, before returning to the caller. A reopened settings window
        // must never overtake an older window's second, still-waiting edit.
        var write = SaveStyleAsync(styleWrites, id, draft);
        styleWrites = write;
        Track(write);
        return write;
    }

    /// <summary>Resets overrides in the same ordering as individual style edits.</summary>
    public Task<JsonObject> ClearStyles()
    {
        var draft = new StyleDraft(++styleRevision, null);
        clearDraft = draft;
        foreach (var window in windows.Values) window.PreviewStyle(null);
        var write = ClearStylesAsync(styleWrites, draft);
        styleWrites = write;
        Track(write);
        return write;
    }

    private async Task SaveStyleAsync(Task previous, ulong id, StyleDraft draft)
    {
        await previous;
        try
        {
            var patch = new JsonObject { ["style"] = draft.Style?.DeepClone() };
            var reply = await app.Engine.CallAsync("cards.update", new JsonObject { ["id"] = id, ["patch"] = patch });
            // card_save_failed means the core accepted the patch but disk persistence
            // is pending. Other failures must remain recoverable across UI windows.
            draft.Accepted = reply.Ok() || reply["error"].Str() == "card_save_failed";
            if (!reply.Ok() && !app.Exiting)
            {
                app.Bar.ShowMessage(Loc.Error(reply["error"].Str("save_failed")), true);
            }
        }
        catch (Exception)
        {
            if (!app.Exiting)
            {
                app.Bar.ShowMessage(Loc.Error("save_failed"), true);
            }
        }
        RetireStyles();
    }

    private async Task<JsonObject> ClearStylesAsync(Task previous, StyleDraft draft)
    {
        await previous;
        JsonObject reply;
        try
        {
            reply = await app.Engine.CallAsync("cards.styles.clear");
            draft.Accepted = reply.Ok() || reply["error"].Str() == "card_save_failed";
            if (draft.Accepted)
            {
                foreach (var id in styleDrafts.Where(pair => pair.Value.Revision <= draft.Revision).Select(pair => pair.Key).ToArray())
                    styleDrafts.Remove(id);
            }
        }
        catch (Exception)
        {
            reply = new JsonObject { ["ok"] = false, ["error"] = "save_failed" };
        }
        RetireStyles();
        return reply;
    }

    private void RetireStyles()
    {
        if (clearDraft is { Accepted: true } clear
            && Cards.All(card => card["style"] is null
                || (styleDrafts.TryGetValue((ulong)card["id"].Num(), out var later) && later.Revision > clear.Revision)))
        {
            clearDraft = null;
        }
        // Keep edits after a rejected clear until it is retried: retrying that clear
        // must replay these newer edits afterwards, even if they were accepted once.
        foreach (var (id, draft) in styleDrafts.ToArray())
        {
            if (!draft.Accepted || clearDraft is { Accepted: false }) continue;
            var card = Cards.FirstOrDefault(card => (ulong)card["id"].Num() == id);
            if (card is not null && JsonNode.DeepEquals(draft.Style, card["style"])) styleDrafts.Remove(id);
        }
    }

    private async Task DrainWritesAsync()
    {
        // Continuations may enqueue another write while a captured batch completes.
        // Inspect the shared list again instead of assuming its first snapshot is final.
        do
        {
            await Task.WhenAll(pendingWrites.ToArray());
        }
        while (!styleWrites.IsCompleted || pendingWrites.Any(write => !write.IsCompleted));
    }

    private void RetryRejectedStyles()
    {
        var retryClear = clearDraft is { Accepted: false } ? clearDraft : null;
        var retryStyles = styleDrafts.Where(pair => !pair.Value.Accepted
            || (retryClear is not null && pair.Value.Revision > retryClear.Revision))
            .Where(pair => clearDraft is null || pair.Value.Revision > clearDraft.Revision)
            .OrderBy(pair => pair.Value.Revision).ToArray();
        // Reset acceptance before starting the clear, which may complete inline.
        // Otherwise its completion could retire a later draft before its replay.
        foreach (var (_, draft) in retryStyles) draft.Accepted = false;
        if (retryClear is not null)
        {
            var clear = ClearStylesAsync(styleWrites, retryClear);
            styleWrites = clear;
            Track(clear);
        }
        foreach (var (id, draft) in retryStyles)
        {
            var write = SaveStyleAsync(styleWrites, id, draft);
            styleWrites = write;
            Track(write);
        }
    }

    public void CloseAll()
    {
        foreach (var window in windows.Values)
        {
            window.CloseCard();
        }
        windows.Clear();
    }

    public void Track(Task write)
    {
        pendingWrites.RemoveAll(task => task.IsCompleted);
        pendingWrites.Add(write);
    }

    public async Task<bool> FlushAsync()
    {
        flushing = true;
        // Freeze all current editors before any await, including when an older
        // settings window still has writes waiting in the shared style queue.
        var flushes = windows.Values.ToArray().Select(window => window.FlushAsync()).ToArray();
        await Task.WhenAll(flushes);
        // Includes writes from cards hidden or archived just before exit.
        await DrainWritesAsync();
        RetryRejectedStyles();
        await DrainWritesAsync();
        // A queued write may have failed after the first flush started. Recheck
        // window pending fields only after the entire shared queue has settled.
        var accepted = await Task.WhenAll(windows.Values.ToArray().Select(window => window.FlushAsync()));
        await DrainWritesAsync();
        // Acceptance only means the core owns the edit. Even with no open windows,
        // dirty cards must remain in the running app until their disk retry succeeds.
        var cards = await app.Engine.CallAsync("cards.list");
        return accepted.All(value => value) && !styleDrafts.Values.Any(draft => !draft.Accepted)
            && clearDraft is not { Accepted: false } && cards.Ok() && !cards["unsaved"].Bool(true);
    }

    public void ResumeEditing()
    {
        flushing = false;
        foreach (var window in windows.Values) window.ResumeEditing();
    }
}
