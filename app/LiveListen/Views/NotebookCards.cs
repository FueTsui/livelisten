using System.Text;
using System.Text.Json.Nodes;
using LiveListen.Controls;
using LiveListen.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace LiveListen.Views;

internal sealed partial class NotebookView
{
    private string cardFilter = "open";
    private StackPanel cardsPanel = null!;
    private ToggleSwitch desktopSwitch = null!;
    private TextBlock cardsStatus = null!;
    private TextBlock markdownStatus = null!;
    private TextBlock shortcutHint = null!;
    private bool cardsPending;
    private bool displayedCards;
    private static readonly string[] rowFields = ["id", "kind", "text", "rtf", "done", "archived", "created", "createdMs"];
    private List<JsonObject> renderedCards = [];
    private string? renderedFilter;
    private readonly Dictionary<ulong, Action<bool>> rowVisibility = [];

    private void BuildCards()
    {
        var page = new StackPanel { Spacing = 4, MaxWidth = 1000, Padding = new Thickness(36, 8, 36, 36) };
        shortcutHint = Secondary();
        RefreshShortcutHint();
        page.Children.Add(shortcutHint);

        displayedCards = app.Cards.Shown;
        desktopSwitch = new ToggleSwitch { IsOn = displayedCards, MinWidth = 0, OnContent = T("cards.desktop"), OffContent = T("cards.desktop") };
        desktopSwitch.Toggled += (_, _) =>
        {
            if (!closed && desktopSwitch.IsOn != displayedCards)
            {
                displayedCards = desktopSwitch.IsOn;
                _ = app.Engine.CallAsync("cards.show", new JsonObject { ["on"] = desktopSwitch.IsOn });
            }
        };

        var newText = new TextBox
        {
            Header = T("cards.new.text"),
            AcceptsReturn = true,
            TextWrapping = TextWrapping.Wrap,
            Width = 300,
            MinHeight = 80,
        };
        var newKind = new RadioButtons { MaxColumns = 2 };
        newKind.Items.Add(T("cards.kind.note"));
        newKind.Items.Add(T("cards.kind.todo"));
        newKind.SelectedIndex = 0;
        var addPanel = new StackPanel { Spacing = 12 };
        addPanel.Children.Add(newText);
        addPanel.Children.Add(newKind);
        var addFlyout = new Flyout
        {
            Content = addPanel,
            ShowMode = Microsoft.UI.Xaml.Controls.Primitives.FlyoutShowMode.Standard,
            AllowFocusOnInteraction = true,
        };
        var create = new Button { Content = T("common.add"), Style = Res<Style>("AccentButtonStyle") };
        var creating = false;
        create.Click += async (_, _) =>
        {
            if (closed || creating) return;
            var text = newText.Text.Trim();
            if (text.Length == 0)
            {
                return;
            }
            if (text.EnumerateRunes().Count() > 4000)
            {
                cardsStatus.Text = Loc.Error("text_too_long");
                return;
            }
            var kind = newKind.SelectedIndex == 1 ? "todo" : "note";
            creating = true;
            create.IsEnabled = false;
            try
            {
                var reply = await app.Engine.CallAsync("cards.add", new JsonObject { ["text"] = text, ["kind"] = kind });
                if (closed) return;
                if (reply.Ok() || reply["error"].Str() is "card_save_failed" or "card_markdown_failed")
                {
                    // The core owns accepted edits even when its disk retry is pending.
                    // Do not erase text the user changed while this request was running.
                    if (newText.Text.Trim() == text) newText.Text = "";
                    addFlyout.Hide();
                }
                cardsStatus.Text = reply.Ok() ? "" : Loc.Error(reply["error"].Str());
            }
            catch (Exception)
            {
                if (!closed) cardsStatus.Text = Loc.Error("save_failed");
            }
            finally
            {
                creating = false;
                if (!closed) create.IsEnabled = true;
            }
        };
        addPanel.Children.Add(create);
        addFlyout.Opening += (_, _) => newCardFlyoutOpen = true;
        addFlyout.Opened += (_, _) => newText.Focus(FocusState.Programmatic);
        addFlyout.Closed += (_, _) =>
        {
            newCardFlyoutOpen = false;
            QueueCardsRefresh();
        };
        var add = new Button
        {
            Content = Horizontal(new FontIcon { Glyph = "", FontSize = 14 }, new TextBlock { Text = T("cards.new") }),
            Flyout = addFlyout,
        };
        AutomationProperties.SetName(add, T("cards.new"));

        var tools = new Grid { Margin = new Thickness(0, 12, 0, 4) };
        tools.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        tools.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var filter = new SelectorBar();
        foreach (var tag in new[] { "open", "done", "archived" })
        {
            var item = new SelectorBarItem { Text = T("cards.filter." + tag), Tag = tag };
            filter.Items.Add(item);
            if (tag == cardFilter)
            {
                filter.SelectedItem = item;
            }
        }
        var switchingFilter = false;
        filter.SelectionChanged += async (sender, _) =>
        {
            if (switchingFilter) return;
            if (sender.SelectedItem is { Tag: string tag } && tag != cardFilter)
            {
                switchingFilter = true;
                try
                {
                    sender.SelectedItem = sender.Items.First(item => item.Tag as string == cardFilter);
                    if (!await FlushAsync() || closed) return;
                    cardFilter = tag;
                    sender.SelectedItem = sender.Items.First(item => item.Tag as string == tag);
                    FillCards();
                }
                finally { switchingFilter = false; }
            }
        };
        tools.Children.Add(filter);
        var openFolder = new Button { Content = T("cards.openFolder") };
        openFolder.Click += (_, _) =>
        {
            try
            {
                var folder = System.IO.Path.Combine(app.ConfigDir, "随手记");
                if (System.IO.Directory.Exists(folder))
                    System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(folder) { UseShellExecute = true });
                else cardsStatus.Text = Loc.Error("card_markdown_failed");
            }
            catch (Exception) { cardsStatus.Text = Loc.Error("card_markdown_failed"); }
        };
        var right = Horizontal(desktopSwitch, openFolder, add);
        right.Spacing = 16;
        right.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(right, 1);
        tools.Children.Add(right);
        page.Children.Add(tools);

        cardsStatus = Secondary();
        page.Children.Add(cardsStatus);
        markdownStatus = Secondary(app.Cards.MarkdownError.Length > 0 ? Loc.Error(app.Cards.MarkdownError) : "");
        markdownStatus.Visibility = app.Cards.MarkdownError.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
        page.Children.Add(markdownStatus);
        cardsPanel = new StackPanel { Spacing = 4 };
        page.Children.Add(cardsPanel);
        pageHost.Children.Add(new ScrollViewer { Content = page, HorizontalScrollMode = ScrollMode.Disabled });

        pageEvents = (name, _) =>
        {
            if (name != "cards.changed")
            {
                return;
            }
            displayedCards = app.Cards.Shown;
            desktopSwitch.IsOn = displayedCards;
            markdownStatus.Text = app.Cards.MarkdownError.Length > 0 ? Loc.Error(app.Cards.MarkdownError) : "";
            markdownStatus.Visibility = app.Cards.MarkdownError.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
            if (Editing())
            {
                cardsPending = true;
            }
            else
            {
                FillCards();
            }
        };
        FillCards();
    }

    public void RefreshShortcutHint()
    {
        if (closed || shortcutHint is null) return;
        var keys = app.Settings.Str("hotkeys.note");
        shortcutHint.Text = keys.Length > 0 ? Loc.F("cards.desc.key", Theme.Keys(keys)) : T("cards.desc");
    }

    private void FillCards()
    {
        if (closed) return;
        if (Editing()) { cardsPending = true; return; }
        cardsPending = false;
        var cards = app.Cards.Cards
            .Where(c => cardFilter switch
            {
                "archived" => c["archived"].Bool(),
                "done" => !c["archived"].Bool() && c["done"].Bool(),
                _ => !c["archived"].Bool() && !c["done"].Bool(),
            })
            .OrderByDescending(c => c["createdMs"].Num())
            .ToList();
        // Desktop movement, title settings and global visibility can update the
        // card book without changing any of the editable rows.
        if (renderedFilter == cardFilter && cards.Count == renderedCards.Count
            && cards.Zip(renderedCards).All(pair => rowFields.All(key => JsonNode.DeepEquals(pair.First[key], pair.Second[key]))))
        {
            foreach (var card in cards)
                if (rowVisibility.TryGetValue((ulong)card["id"].Num(), out var update))
                    update(card["visible"].Bool());
            return;
        }
        renderedFilter = cardFilter;
        renderedCards = cards;
        cardsPanel.Children.Clear();
        cardEditors.Clear();
        cardFlushers.Clear();
        rowVisibility.Clear();
        if (cards.Count == 0)
        {
            cardsPanel.Children.Add(Secondary(T("cards.none." + cardFilter)));
        }
        foreach (var card in cards)
        {
            cardsPanel.Children.Add(CardRow(card));
        }
    }

    private Border CardRow(JsonObject card)
    {
        var id = (ulong)card["id"].Num();
        var todo = card["kind"].Str() == "todo";
        var archived = card["archived"].Bool();
        var text = new NoteEditor();
        text.Apply(Theme.CardLook(app.Settings.Str("appearance.theme", "system"), _ => null));
        text.Load(card["text"].Str(), card["rtf"]?.GetValue<string>());
        cardEditors.Add(text);
        var savedContent = text.GetContent();
        var pending = new Dictionary<string, (long Revision, JsonNode? Value)>();
        long revision = 0;
        Task writes = Task.CompletedTask;
        async Task SaveAfter(Task previous, JsonObject patch, long sentRevision, (string Text, string Rtf)? sentContent)
        {
            await previous;
            try
            {
                var reply = await app.Engine.CallAsync("cards.update", new JsonObject { ["id"] = id, ["patch"] = patch });
                if (reply.Ok() || reply["error"].Str() is "card_save_failed" or "card_markdown_failed")
                {
                    if (sentContent is { } acceptedContent) savedContent = acceptedContent;
                    foreach (var key in pending.Where(pair => pair.Value.Revision <= sentRevision).Select(pair => pair.Key).ToArray())
                        pending.Remove(key);
                    if (pending.Count == 0) dirtyEditors.Remove(text);
                }
                if (!closed) cardsStatus.Text = reply.Ok() ? "" : Loc.Error(reply["error"].Str("save_failed"));
            }
            catch (Exception) { if (!closed) cardsStatus.Text = Loc.Error("save_failed"); }
            QueueCardsRefresh();
        }
        void Patch(JsonObject patch)
        {
            if (!text.TryGetContent(out var content))
            {
                cardsStatus.Text = Loc.Error("bad_rich_text");
                return;
            }
            var plannedContent = pending.TryGetValue("text", out var queuedText) && pending.TryGetValue("rtf", out var queuedRtf)
                ? (queuedText.Value?.GetValue<string>() ?? "", queuedRtf.Value?.GetValue<string>() ?? "") : savedContent;
            if (content.Text.Length > 0 && content != plannedContent)
            {
                patch["text"] = content.Text;
                patch["rtf"] = content.Rtf;
            }
            if (patch.Count == 0 && !writes.IsCompleted) return;
            foreach (var (key, field) in pending)
                if (!patch.ContainsKey(key)) patch[key] = field.Value?.DeepClone();
            if (patch.Count == 0) return;
            var sentRevision = ++revision;
            foreach (var field in patch) pending[field.Key] = (sentRevision, field.Value?.DeepClone());
            dirtyEditors.Add(text);
            (string Text, string Rtf)? sentContent = patch.ContainsKey("text")
                ? (patch["text"].Str(), patch["rtf"].Str()) : null;
            writes = SaveAfter(writes, patch, sentRevision, sentContent);
            app.Cards.Track(writes);
        }
        cardFlushers.Add(async () =>
        {
            if (!text.CanSave) return false;
            text.FinishEditing();
            Patch(new JsonObject());
            await writes;
            if (pending.Count > 0)
            {
                Patch(new JsonObject());
                await writes;
            }
            return pending.Count == 0;
        });

        var grid = new Grid { ColumnSpacing = 12 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var displayedDone = card["done"].Bool();
        var done = new CheckBox { IsChecked = displayedDone, MinWidth = 0, VerticalAlignment = VerticalAlignment.Top };
        AutomationProperties.SetName(done, T("card.done"));
        void ChangedDone()
        {
            var value = done.IsChecked == true;
            if (closed || value == displayedDone) return;
            displayedDone = value;
            Patch(new JsonObject { ["done"] = value });
        }
        done.Checked += (_, _) => ChangedDone();
        done.Unchecked += (_, _) => ChangedDone();
        grid.Children.Add(done);

        var stack = new StackPanel { Spacing = 4 };
        text.EditingEnded += () =>
        {
            if (text.GetContent().Text.Length == 0)
            {
                if (pending.TryGetValue("text", out var queuedText) && pending.TryGetValue("rtf", out var queuedRtf))
                    text.Load(queuedText.Value?.GetValue<string>() ?? savedContent.Text, queuedRtf.Value?.GetValue<string>() ?? savedContent.Rtf);
                else text.Load(savedContent.Text, savedContent.Rtf);
            }
            else
            {
                Patch(new JsonObject());
            }
            QueueCardsRefresh();
        };
        stack.Children.Add(text.View);
        var meta = T(todo ? "cards.kind.todo" : "cards.kind.note") + " · " + Format.When(card["created"].Str());
        var metadata = Secondary(meta);
        stack.Children.Add(metadata);
        Grid.SetColumn(stack, 1);
        grid.Children.Add(stack);

        var menu = new MenuFlyout();
        text.TrackExternalFlyout(menu);
        menu.Opening += (_, _) =>
        {
            // The active editor may belong to an earlier row. Keep its footer
            // until this menu closes so the anchor cannot move during the click.
            foreach (var editor in cardEditors.Where(editor => editor.IsEditing))
                editor.TrackExternalFlyout(menu, isOpening: true);
        };
        menu.Closed += (_, _) => QueueCardsRefresh();
        void Item(string label, string glyph, Action run)
        {
            var item = new MenuFlyoutItem { Text = label, Icon = new FontIcon { Glyph = glyph } };
            item.Click += (_, _) => run();
            menu.Items.Add(item);
        }
        if (!archived)
        {
            var visible = card["visible"].Bool();
            var icon = new FontIcon();
            var toggle = new MenuFlyoutItem { Icon = icon };
            void UpdateVisibility(bool value)
            {
                visible = value;
                metadata.Text = visible && app.Cards.Shown ? meta + " · " + T("cards.onDesktop") : meta;
                toggle.Text = T(visible ? "cards.hideDesktop" : "cards.showDesktop");
                icon.Glyph = visible ? "" : "";
            }
            rowVisibility[id] = UpdateVisibility;
            UpdateVisibility(visible);
            toggle.Click += (_, _) =>
            {
                Patch(new JsonObject { ["visible"] = !visible });
                if (!visible && !app.Cards.Shown)
                {
                    _ = app.Engine.CallAsync("cards.show", new JsonObject { ["on"] = true });
                }
            };
            menu.Items.Add(toggle);
        }
        Item(T(todo ? "cards.toNote" : "cards.toTodo"), todo ? "" : "", () =>
            Patch(new JsonObject { ["kind"] = todo ? "note" : "todo" }));
        if (!archived)
        {
            Item(T("card.menu.style"), "\uE790", () => app.OpenCardStyle(id));
        }
        Item(T(archived ? "cards.restore" : "card.archive"), archived ? "" : "", () =>
            Patch(new JsonObject { ["archived"] = !archived }));
        var when = WhenPicker.Create(() => (long)card["createdMs"].Num(), () => root.ActualTheme,
            ms => Patch(new JsonObject { ["createdMs"] = ms }));
        text.TrackExternalFlyout(when);
        when.Opening += (_, _) =>
        {
            foreach (var editor in cardEditors.Where(editor => editor.IsEditing))
                editor.TrackExternalFlyout(when, isOpening: true);
        };
        when.Closed += (_, _) => QueueCardsRefresh();
        Item(T("card.menu.date"), "", () =>
        {
            // Protect the editor during the gap between the menu closing and
            // the date picker opening, so saving cannot replace its anchor.
            text.TrackExternalFlyout(when, isOpening: true);
            foreach (var editor in cardEditors.Where(editor => editor.IsEditing))
                editor.TrackExternalFlyout(when, isOpening: true);
            owner.DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () => { if (!closed) when.ShowAt(text.View); });
        });
        menu.Items.Add(new MenuFlyoutSeparator());
        Item(T("cards.delete"), "", () => _ = DeleteCard(id));
        var more = new Button
        {
            Content = new FontIcon { Glyph = "", FontSize = 14 },
            Flyout = menu,
            AllowFocusOnInteraction = false,
            VerticalAlignment = VerticalAlignment.Top,
        };
        AutomationProperties.SetName(more, T("cards.more"));
        ToolTipService.SetToolTip(more, T("cards.more"));
        Grid.SetColumn(more, 2);
        grid.Children.Add(more);
        return new Border { Style = Res<Style>("CardStyle"), Child = grid };
    }

    private async Task DeleteCard(ulong id)
    {
        var dialog = new ContentDialog
        {
            XamlRoot = root.XamlRoot,
            RequestedTheme = root.ActualTheme,
            Title = T("cards.delete.title"),
            Content = T("cards.delete.desc"),
            PrimaryButtonText = T("cards.delete"),
            CloseButtonText = T("common.cancel"),
            DefaultButton = ContentDialogButton.Close,
        };
        if (await dialog.ShowAsync() != ContentDialogResult.Primary || closed)
        {
            return;
        }
        if (!await FlushAsync() || closed) return;
        var reply = await app.Cards.DeleteAsync(id);
        if (closed) return;
        cardsStatus.Text = reply.Ok() ? "" : Loc.Error(reply["error"].Str());
    }
}
