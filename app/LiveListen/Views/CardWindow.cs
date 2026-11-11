using System.Runtime.InteropServices;
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
using static LiveListen.Interop.Win32;

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
    private readonly CardCheckBox doneBox = new();
    private readonly TextBlock date = new() { FontSize = 12, VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis, IsHitTestVisible = false };
    private readonly Grid handle = new() { Background = new SolidColorBrush(Palette.Transparent), MinHeight = 28 };
    private readonly GlyphButton archive = new("", Loc.T("card.archive"), size: 26, iconSize: 12);
    private readonly GlyphButton hide = new("", Loc.T("card.hide"), size: 26, iconSize: 10);
    private readonly NoteEditor editor = new(requireContentPress: true);
    private readonly SolidColorBrush secondaryBrush = new();
    private (string Text, string Rtf) loadedContent;
    private (string? Text, string? Rtf) loadedSource;

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

        editor.Changed += OnEditorChanged;
        editor.EditingEnded += SaveText;
        Activated += (_, e) =>
        {
            if (e.WindowActivationState == WindowActivationState.Deactivated && !editor.HasOpenFlyout) editor.FinishEditing();
        };

        var header = new Grid { ColumnSpacing = 6, Background = new SolidColorBrush(Palette.Transparent) };
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
        AutomationProperties.SetName(handle, Loc.T("bar.drag"));
        handle.Children.Add(date);
        Grid.SetColumn(handle, 1);
        Grid.SetColumn(archive.View, 2);
        Grid.SetColumn(hide.View, 3);
        header.Children.Add(lead);
        header.Children.Add(handle);
        header.Children.Add(archive.View);
        header.Children.Add(hide.View);
        void ShowHeaderActions(bool visible)
        {
            archive.View.Opacity = hide.View.Opacity = visible ? 1 : 0;
            archive.View.IsHitTestVisible = hide.View.IsHitTestVisible = visible;
        }
        ShowHeaderActions(false);
        header.PointerEntered += (_, _) => ShowHeaderActions(true);
        header.PointerExited += (_, _) => ShowHeaderActions(false);
        ToolTipService.SetToolTip(archive.View, Loc.T("card.archive"));
        ToolTipService.SetToolTip(hide.View, Loc.T("card.hide"));
        archive.Click += () => Patch(new JsonObject { ["archived"] = true });
        hide.Click += () => Patch(new JsonObject { ["visible"] = false });
        _ = new WindowDrag(this, handle, SavePosition) { Snap = SnapTo };
        ToolTipService.SetToolTip(handle, Loc.T("card.header.tip"));
        var headerMenu = CardMenu();
        editor.TrackExternalFlyout(headerMenu);
        header.ContextFlyout = handle.ContextFlyout = headerMenu;

        body.Children.Add(header);
        body.Children.Add(editor.View);
        frame.Children.Add(surfaceHost);
        frame.Children.Add(body);
        root.Children.Add(frame);
        Content = root;
        surface = new Surface(surfaceHost);
        Overlay.Prepare(this, floating: false);
        CardSmokeProbe.Attach(this, root, Id, () => editor.IsEditing);
        AppWindow.Closing += OnClosing;
        var scale = Overlay.Scale(this);
        AppWindow.Resize(new SizeInt32((int)((CardWidth + 32) * scale), (int)(120 * scale)));
        Refresh();
        Update(card);
        root.Loaded += (_, _) => QueueFit();
        Closed += (_, _) => closing = true;
    }

    public ulong Id { get; }
    public bool IsCardVisible => Overlay.IsVisible(this);
    public bool IsEmptyDraft => !closing && IsCardVisible && editor.IsEmpty;
    public bool HasOpenFlyout => editor.HasOpenFlyout;
    public (string Text, string? Rtf) SavedText => (card["text"].Str(), card["rtf"]?.GetValue<string>());

    private void OnClosing(AppWindow sender, AppWindowClosingEventArgs e)
    {
        if (app.Exiting || closing) return;
        // Alt+F4 hides the card from the desktop; it stays in the list.
        e.Cancel = true;
        Patch(new JsonObject { ["visible"] = false });
    }

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
        var x = rect.X + move.X;
        var y = rect.Y + move.Y;
        // The drawn card stays inside the work area, so it cannot be lost past an edge.
        x = Math.Clamp(x, work.X, Math.Max(work.X, work.X + work.Width - rect.Width));
        y = Math.Clamp(y, work.Y, Math.Max(work.Y, work.Y + work.Height - rect.Height));
        return new PointInt32(p.X + x - rect.X, p.Y + y - rect.Y);
    }

    /// <summary>Places the card (its saved spot, or a free spot near the top right) and shows it.</summary>
    public void ShowCard(int index)
    {
        Fit();
        if (card["x"] is JsonNode x && card["y"] is JsonNode y)
        {
            Overlay.PlaceAt(this, (int)x.Num(), (int)y.Num());
            // A saved place outside the screen was moved in; do not land on another card.
            var moved = AppWindow.Position;
            if (moved.X != (int)x.Num() || moved.Y != (int)y.Num()) AvoidOtherCards();
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

    private void AvoidOtherCards()
    {
        var others = app.Cards.CardRects(Id);
        var step = (int)(36 * Overlay.Scale(this));
        for (var tries = 0; tries < 12; tries++)
        {
            var mine = CardRect;
            if (!others.Any(o => o.X < mine.X + mine.Width && mine.X < o.X + o.Width
                && o.Y < mine.Y + mine.Height && mine.Y < o.Y + o.Height)) return;
            var before = AppWindow.Position;
            Overlay.PlaceAt(this, before.X, before.Y + step);
            if (AppWindow.Position.Y == before.Y) Overlay.PlaceAt(this, before.X - step, before.Y);
            if (AppWindow.Position == before) return;
        }
    }

    public void ShowExisting()
    {
        if (IsCardVisible) return;
        Fit();
        // A reused window kept its old place in the window order and on a display that
        // may be gone: bring it back where the user can see it, without taking focus.
        KeepOnScreen();
        Overlay.ShowQuiet(this);
        Overlay.RaiseQuiet(this);
    }

    /// <summary>Moves the card back inside the nearest display's work area.</summary>
    public void KeepOnScreen()
    {
        var position = AppWindow.Position;
        Overlay.PlaceAt(this, position.X, position.Y);
    }

    /// <summary>Commits the text being edited without waiting; used when Windows shuts down.</summary>
    public void CommitDraft() => editor.FinishEditing();

    public void HideCard()
    {
        if (app.Cards.StopWatchingEmpty(this)) LoadText();
        Overlay.Hide(this);
    }

    public void RestoreClearedText()
    {
        if (!closing && !editor.IsEditing && editor.IsEmpty) LoadText();
    }

    public void EditNewCard()
    {
        Overlay.BringToFront(this);
        if (root.IsLoaded) editor.BeginEditing();
        else root.Loaded += EditNewCardWhenLoaded;
    }

    private void EditNewCardWhenLoaded(object sender, RoutedEventArgs e)
    {
        root.Loaded -= EditNewCardWhenLoaded;
        editor.BeginEditing();
    }

    public bool CloseCard()
    {
        editor.FinishEditing();
        if (editor.IsEditing)
        {
            app.Bar.ShowMessage(Loc.Error("bad_rich_text"), true);
            return false;
        }
        if (pendingFields.Count > 0 || !saves.IsCompleted) return false;
        CloseWindow();
        return true;
    }

    /// <summary>Closes an already-flushed card after its record has been deleted.</summary>
    public void CloseAfterDeletion() => CloseWindow();

    private void CloseWindow()
    {
        closing = true;
        app.Cards.StopWatchingEmpty(this);
        AppWindow.Closing -= OnClosing;
        SystemBackdrop = null;
        surface.ReleaseShadow();
        Content = null;
        Close();
    }

    /// <summary>Commits the focused editor and drains patches before the engine can stop.</summary>
    public async Task<bool> FlushAsync()
    {
        if (editor.IsEditing && !editor.CanSave) return false;
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
        editor.Pause();
        root.IsHitTestVisible = false;
    }

    public void ResumeEditing()
    {
        editor.Resume();
        root.IsHitTestVisible = true;
    }

    public void Update(JsonObject value)
    {
        if (pendingFields.Count == 0 && (value["text"].Str(), value["rtf"].Str()) == loadedSource
            && JsonNode.DeepEquals(card, value)) return;
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
        RefreshHeader();
        if (!editor.IsEditing && (card["text"].Str(), card["rtf"].Str()) != loadedSource)
        {
            LoadText();
        }
        editor.View.Opacity = done ? 0.55 : 1;
        syncing = false;
        QueueFit();
    }

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
        var nextLook = Theme.Look(app.Settings, "cards", previewStyle ?? app.Cards.EffectiveStyle(Id));
        if (nextLook != look)
        {
            look = nextLook;
            root.RequestedTheme = look.Dark ? ElementTheme.Dark : ElementTheme.Light;
            frame.Margin = new Thickness(look.ShadowMargin);
            surface.Apply(look);
            body.Padding = new Thickness(Math.Max(8, look.Padding), Math.Max(4, look.Padding - 6), Math.Max(6, look.Padding - 4), Math.Max(8, look.Padding));
            secondaryBrush.Color = look.Secondary;
            editor.Apply(look);
            date.Foreground = secondaryBrush;
            kindIcon.Foreground = secondaryBrush;
            archive.Apply(look);
            hide.Apply(look);
        }
        RefreshHeader();
        QueueFit();
    }

    private void RefreshHeader()
    {
        var style = previewStyle ?? app.Cards.EffectiveStyle(Id) ?? app.Settings.Get("appearance.cards") as JsonObject;
        var mode = CardHeaderInfo.Mode(style?["cardHeader"].Str());
        date.Text = CardHeaderInfo.Render(card["created"].Str(), mode,
            style?["cardHeaderDate"].Str(), style?["cardHeaderTitle"].Str(),
            Loc.T(card["kind"].Str() == "todo" ? "cards.kind.todo" : "cards.kind.note"), Loc.English);
        // Keep the handle's geometry and state/action buttons even without a title.
        date.Visibility = mode == "none" ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>The right-click menu; it is rebuilt each time so it matches the card.</summary>
    private MenuFlyout CardMenu()
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
            Item(Loc.T("cards.new"), "\uE710", app.Cards.CreateDesktopCard);
            menu.Items.Add(new MenuFlyoutSeparator());
            Item(Loc.T(todo ? "cards.toNote" : "cards.toTodo"), todo ? "\uE70B" : "\uE73A",
                () => Patch(new JsonObject { ["kind"] = todo ? "note" : "todo" }));
            menu.Items.Add(new MenuFlyoutSeparator());
            Item(Loc.T("card.hide"), "\uED1A", () => Patch(new JsonObject { ["visible"] = false }));
            menu.Items.Add(new MenuFlyoutSeparator());
            var colors = new MenuFlyoutSubItem
            {
                Text = Loc.T("card.palette"),
                Icon = new FontIcon { Glyph = "\uE790" },
            };
            var ownStyle = app.Cards.EffectiveStyle(Id);
            var shared = new ToggleMenuFlyoutItem
            {
                Text = Loc.T("card.palette.default"),
                IsChecked = ownStyle is null,
            };
            shared.Click += (_, _) => _ = SetStyle(null);
            colors.Items.Add(shared);
            colors.Items.Add(new MenuFlyoutSeparator());
            foreach (var preset in CardPalette.All)
            {
                var item = new ToggleMenuFlyoutItem
                {
                    Text = Loc.T("card.palette." + preset.Key),
                    IsChecked = preset.Matches(ownStyle),
                    Icon = new FontIcon
                    {
                        Glyph = "\uEA3B",
                        FontSize = 14,
                        Foreground = new SolidColorBrush(Theme.Parse(preset.Border) ?? look.Secondary),
                    },
                };
                item.Click += (_, _) =>
                {
                    var basis = app.Cards.EffectiveStyle(Id)
                        ?? app.Settings.Get("appearance.cards") as JsonObject ?? new JsonObject();
                    _ = SetStyle(preset.Apply(basis));
                };
                colors.Items.Add(item);
            }
            menu.Items.Add(colors);
            Item(Loc.T("card.menu.settings"), "\uE713", () => app.OpenNotebook());
        };
        return menu;
    }

    private void OnEditorChanged()
    {
        QueueFit();
        if (!editor.IsEditing || closing) return;
        if (editor.IsEmpty) app.Cards.WatchEmpty(this);
        else app.Cards.StopWatchingEmpty(this);
    }

    private void SaveText()
    {
        if (!editor.TryGetContent(out var content)) return;
        if (content.Text.Length > 0 && content != loadedContent)
        {
            Patch(new JsonObject(), content);
        }
    }

    private void LoadText()
    {
        app.Cards.StopWatchingEmpty(this);
        editor.Load(card["text"].Str(), card["rtf"]?.GetValue<string>());
        loadedContent = editor.GetContent();
        loadedSource = (card["text"].Str(), card["rtf"].Str());
    }

    private void Patch(JsonObject patch, (string Text, string Rtf)? validatedDraft = null)
    {
        if (syncing)
        {
            return;
        }
        var draft = validatedDraft.GetValueOrDefault();
        var hasDraft = validatedDraft.HasValue || editor.IsEditing;
        var canSave = !hasDraft || validatedDraft.HasValue || editor.TryGetContent(out draft);
        if (!canSave && (patch["visible"]?.GetValue<bool>() == false || patch["archived"]?.GetValue<bool>() == true))
        {
            app.Bar.ShowMessage(Loc.Error("bad_rich_text"), true);
            return;
        }
        // Hiding or archiving can close the window before LostFocus. Include its draft.
        if (hasDraft && canSave && draft.Text.Length > 0 && draft != loadedContent)
        {
            patch["text"] = draft.Text;
            patch["rtf"] = draft.Rtf;
            loadedContent = draft;
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
            if (reply.Ok() || reply["error"].Str() is "card_save_failed" or "card_markdown_failed")
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
            if (!closing && IsCardVisible) Fit();
        })) fitQueued = false;
    }

    private void Fit() => Overlay.Fit(this, root, CardWidth + 2 * look.ShadowMargin, keepTop: true);
}

/// <summary>Keeps one window for each card that should be on the desktop.</summary>
internal sealed class CardsHost(App app)
{
    private const int MaxIndividuallyHiddenWindows = 4;
    private readonly Dictionary<ulong, CardWindow> windows = [];
    private HashSet<ulong> retainedWindows = [];
    private readonly HashSet<ulong> closingCards = [];
    private readonly HashSet<ulong> deletingCards = [];
    private readonly List<Task> pendingDeletes = [];
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
    private bool creatingCard;
    private static CardsHost? clickWatcher;
    private CardWindow? emptyCard;
    private nint emptyClickHook;
    private bool emptyClickQueued;
    private long emptyEditRevision;

    public bool Shown { get; private set; }
    public string MarkdownError { get; private set; } = "";

    public IReadOnlyList<JsonObject> Cards { get; private set; } = [];

    /// <summary>Restores the cards saved before the last exit.</summary>
    public void Load() => Apply(app.Engine.Call("cards.list"));

    /// <summary>Applies a <c>cards.list</c> reply or a <c>cards.changed</c> event.</summary>
    public void Apply(JsonObject e)
    {
        Shown = e["showOnDesktop"].Bool();
        MarkdownError = e["markdownError"].Str();
        Cards = (e["cards"] as JsonArray)?.OfType<JsonObject>().ToList() ?? [];
        RetireStyles();
        var eligible = Cards.Where(c => c["visible"].Bool() && !c["archived"].Bool()).ToList();
        var eligibleIds = eligible.Select(c => (ulong)c["id"].Num()).ToHashSet();
        retainedWindows = new HashSet<ulong>(eligibleIds);
        retainedWindows.UnionWith(Cards.Where(c => !c["visible"].Bool() && !c["archived"].Bool())
            .Select(c => (ulong)c["id"].Num()).Where(windows.ContainsKey).Take(MaxIndividuallyHiddenWindows));
        foreach (var id in windows.Keys.Where(id => !retainedWindows.Contains(id)
            || (!Shown || !eligibleIds.Contains(id)) && windows[id].IsCardVisible).ToList())
        {
            // The delete owner has already frozen and drained this editor. Its completion
            // closes the window without trying to save to the now-deleted core record.
            if (deletingCards.Contains(id)) continue;
            _ = HideOrCloseAsync(id, windows[id]);
        }
        var index = 0;
        foreach (var c in eligible)
        {
            var id = (ulong)c["id"].Num();
            if (deletingCards.Contains(id)) continue;
            if (windows.TryGetValue(id, out var window))
            {
                window.Update(c);
                if (Shown && !closingCards.Contains(id)) window.ShowExisting();
            }
            else if (Shown)
            {
                window = new CardWindow(app, c);
                windows[id] = window;
                window.ShowCard(index);
                if (flushing) window.PauseEditing();
            }
            index++;
        }
        if (Shown && !flushing && e["editCard"]?.GetValue<ulong>() is { } idToEdit
            && windows.TryGetValue(idToEdit, out var newCard)) newCard.EditNewCard();
        ScheduleRelease();
    }

    private Microsoft.UI.Dispatching.DispatcherQueueTimer? releaseTimer;

    /// <summary>
    /// Hidden windows are kept so that showing them again is immediate. When they stay hidden
    /// for ten minutes, their windows are closed; the cards themselves are untouched.
    /// </summary>
    private void ScheduleRelease()
    {
        if (releaseTimer is null)
        {
            releaseTimer = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread().CreateTimer();
            releaseTimer.Interval = TimeSpan.FromMinutes(10);
            releaseTimer.IsRepeating = false;
            releaseTimer.Tick += (_, _) => ReleaseHidden();
        }
        releaseTimer.Stop();
        if (windows.Values.Any(window => !window.IsCardVisible)) releaseTimer.Start();
    }

    private void ReleaseHidden()
    {
        if (app.Exiting || flushing) return;
        foreach (var (id, window) in windows.ToArray())
        {
            if (window.IsCardVisible || closingCards.Contains(id) || deletingCards.Contains(id)) continue;
            if (window.CloseCard()) windows.Remove(id);
        }
    }

    public void CreateDesktopCard()
    {
        if (app.Exiting || !Shown || creatingCard) return;
        creatingCard = true;
        Track(CreateDesktopCardAsync());
    }

    private async Task CreateDesktopCardAsync()
    {
        try
        {
            var reply = await app.Engine.CallAsync("cards.add", new JsonObject { ["editOnDesktop"] = true });
            // Save errors are also emitted by the engine, with the unsaved card still available.
            var error = reply["error"].Str("save_failed");
            if (!reply.Ok() && !app.Exiting && error is not ("card_save_failed" or "card_markdown_failed"))
                app.Bar.ShowMessage(Loc.Error(error), true);
        }
        catch (Exception)
        {
            if (!app.Exiting) app.Bar.ShowMessage(Loc.Error("save_failed"), true);
        }
        finally { creatingCard = false; }
    }

    // Watch one cleared draft at a time. No click hook is needed while its text is nonempty.
    public unsafe void WatchEmpty(CardWindow window)
    {
        if (app.Exiting || !Shown || ReferenceEquals(emptyCard, window)) return;
        if (emptyClickHook == 0)
        {
            emptyClickHook = SetWindowsHookEx(WH_MOUSE_LL, &OnEmptyCardMouse, GetModuleHandle(0), 0);
            if (emptyClickHook == 0) return;
            clickWatcher = this;
        }
        var previous = emptyCard;
        emptyCard = window;
        emptyEditRevision++;
        previous?.RestoreClearedText();
    }

    public bool StopWatchingEmpty(CardWindow window)
    {
        if (!ReferenceEquals(emptyCard, window)) return false;
        emptyCard = null;
        emptyEditRevision++;
        if (emptyClickHook != 0) UnhookWindowsHookEx(emptyClickHook);
        emptyClickHook = 0;
        clickWatcher = null;
        return true;
    }

    [UnmanagedCallersOnly]
    private static unsafe nint OnEmptyCardMouse(int code, nint message, nint data)
    {
        try
        {
            if (code == HC_ACTION && (uint)message is WM_LBUTTONDOWN or WM_RBUTTONDOWN or WM_MBUTTONDOWN or WM_XBUTTONDOWN)
            {
                var info = (LiveListen.Interop.MSLLHOOKSTRUCT*)data;
                if (info->dwExtraInfo != CoreInputMarker) clickWatcher?.OnPossibleOutsideClick(info->pt);
            }
        }
        catch { /* Never unwind through the Windows input hook. */ }
        return CallNextHookEx(0, code, message, data);
    }

    private void OnPossibleOutsideClick(LiveListen.Interop.POINT point)
    {
        if (emptyClickQueued || emptyCard is not { } window || app.Exiting || deletingCards.Contains(window.Id)) return;
        var hit = WindowFromPoint(point);
        if (hit == 0) return;
        var target = GetAncestor(hit, GA_ROOT);
        var owner = GetAncestor(hit, GA_ROOTOWNER);
        foreach (var cardWindow in windows.Values)
        {
            var hwnd = Overlay.Handle(cardWindow);
            if (hwnd == target || hwnd == owner) return;
        }
        // WinUI flyouts can have a separate top-level popup with no card owner.
        GetWindowThreadProcessId(target, out var pid);
        if (pid == (uint)Environment.ProcessId && windows.Values.Any(card => card.HasOpenFlyout)) return;
        var revision = emptyEditRevision;
        emptyClickQueued = window.DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () =>
        {
            emptyClickQueued = false;
            if (revision == emptyEditRevision) _ = DeleteEmptyCardAsync(window);
        });
    }

    private async Task DeleteEmptyCardAsync(CardWindow window)
    {
        if (app.Exiting || !Shown || !window.IsEmptyDraft) return;
        var reply = await DeleteAsync(window.Id, window.SavedText);
        if (!reply.Ok() && !app.Exiting)
        {
            if (reply["error"].Str() == "card_changed")
            {
                StopWatchingEmpty(window);
                window.RestoreClearedText();
            }
            app.Bar.ShowMessage(Loc.Error(reply["error"].Str("save_failed")), true);
        }
    }

    private async Task HideOrCloseAsync(ulong id, CardWindow window)
    {
        if (!closingCards.Add(id)) return;
        try
        {
            if (deletingCards.Contains(id)) return;
            if (!await window.FlushAsync())
            {
                window.ResumeEditing();
                app.Bar.ShowMessage(Loc.Error("save_failed"), true);
                return;
            }
            if (deletingCards.Contains(id)) return;
            // Keep native state across global visibility changes and a bounded
            // number of individual hides. Re-check the latest state after flushing.
            if (retainedWindows.Contains(id))
            {
                if (Shown && Cards.Any(card => (ulong)card["id"].Num() == id && card["visible"].Bool() && !card["archived"].Bool()))
                    window.ShowExisting();
                else window.HideCard();
                window.ResumeEditing();
                return;
            }
            if (window.CloseCard() && windows.TryGetValue(id, out var current) && ReferenceEquals(current, window))
                windows.Remove(id);
            else window.ResumeEditing();
        }
        catch (Exception)
        {
            window.ResumeEditing();
            if (!app.Exiting) app.Bar.ShowMessage(Loc.Error("save_failed"), true);
        }
        finally { closingCards.Remove(id); }
    }

    /// <summary>Drains every owner of a card before deleting its core record.</summary>
    public Task<JsonObject> DeleteAsync(ulong id, (string Text, string? Rtf)? expected = null)
    {
        if (!deletingCards.Add(id)) return Task.FromResult(DeleteFailure());
        var task = DeleteCoreAsync(id, expected);
        pendingDeletes.RemoveAll(write => write.IsCompleted);
        pendingDeletes.Add(task);
        return task;
    }

    private static JsonObject DeleteFailure() => new() { ["ok"] = false, ["error"] = "save_failed" };

    private async Task<JsonObject> DeleteCoreAsync(ulong id, (string Text, string? Rtf)? expected)
    {
        windows.TryGetValue(id, out var window);
        var deleted = false;
        try
        {
            if (window is not null && !await window.FlushAsync()) return DeleteFailure();
            await DrainWritesAsync();
            RetryRejectedStyles();
            await DrainWritesAsync();
            if (styleDrafts.TryGetValue(id, out var style) && !style.Accepted
                || clearDraft is { Accepted: false }) return DeleteFailure();
            // Accepted text can still be waiting for a disk retry. Keep it until it has
            // a durable source before accepting a destructive operation on that card.
            var snapshot = await app.Engine.CallAsync("cards.list");
            if (!snapshot.Ok() || snapshot["unsaved"].Bool(true)) return DeleteFailure();

            var request = new JsonObject { ["id"] = id };
            if (expected is { } source)
                request["expected"] = new JsonObject { ["text"] = source.Text, ["rtf"] = source.Rtf };
            var reply = await app.Engine.CallAsync("cards.delete", request);
            deleted = reply.Ok() || reply["error"].Str() is "card_save_failed" or "card_markdown_failed";
            if (deleted)
            {
                styleDrafts.Remove(id);
                if (window is not null && windows.TryGetValue(id, out var current) && ReferenceEquals(current, window))
                {
                    window.CloseAfterDeletion();
                    windows.Remove(id);
                }
            }
            return reply;
        }
        catch (Exception)
        {
            return DeleteFailure();
        }
        finally
        {
            deletingCards.Remove(id);
            if (!deleted && window is not null)
            {
                var latest = Cards.FirstOrDefault(card => (ulong)card["id"].Num() == id);
                if (latest is not null) window.Update(latest);
                window.ResumeEditing();
            }
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
        // A delayed appearance-page timer must not enqueue a new write after deletion
        // has drained this card, or leave an unknown-card draft blocking later shutdown.
        if (deletingCards.Contains(id) || !Cards.Any(card => (ulong)card["id"].Num() == id))
        {
            if (!app.Exiting) app.Bar.ShowMessage(Loc.Error("unknown_card"), true);
            return Task.CompletedTask;
        }
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
            // Persistence errors still mean the core accepted the patch; JSON or
            // daily Markdown output is pending. Rejected edits remain recoverable.
            draft.Accepted = reply.Ok() || reply["error"].Str() is "card_save_failed" or "card_markdown_failed";
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
            draft.Accepted = reply.Ok() || reply["error"].Str() is "card_save_failed" or "card_markdown_failed";
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

    /// <summary>The displays changed: no visible card may stay outside them.</summary>
    public void KeepOnScreen()
    {
        foreach (var window in windows.Values.Where(window => window.IsCardVisible)) window.KeepOnScreen();
    }

    /// <summary>Starts saving every draft at once, without waiting for the replies.</summary>
    public void CommitDrafts()
    {
        foreach (var window in windows.Values.ToArray())
        {
            try { window.CommitDraft(); }
            catch (Exception) { }
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
        // A delete confirmed in the notebook may still be draining the desktop editor.
        // Do not stop the engine while that accepted operation is in flight.
        await Task.WhenAll(pendingDeletes.ToArray());
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
