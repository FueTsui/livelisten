using System.Text.Json.Nodes;
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
    private bool cardsPending;
    private bool displayedCards;

    private void BuildCards()
    {
        var page = new StackPanel { Spacing = 4, MaxWidth = 1000, Padding = new Thickness(36, 8, 36, 36) };
        var keys = app.Settings.Str("hotkeys.note");
        page.Children.Add(Secondary(keys.Length > 0 ? Loc.F("cards.desc.key", Theme.Keys(keys)) : T("cards.desc")));

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
        var addFlyout = new Flyout { Content = addPanel };
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
            var kind = newKind.SelectedIndex == 1 ? "todo" : "note";
            creating = true;
            create.IsEnabled = false;
            try
            {
                var reply = await app.Engine.CallAsync("cards.add", new JsonObject { ["text"] = text, ["kind"] = kind });
                if (closed) return;
                if (reply.Ok() || reply["error"].Str() == "card_save_failed")
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
        addFlyout.Opened += (_, _) => newText.Focus(FocusState.Programmatic);
        addFlyout.Closed += (_, _) =>
        {
            if (!closed && cardsPending && !Editing()) FillCards();
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
        filter.SelectionChanged += (sender, _) =>
        {
            if (sender.SelectedItem is { Tag: string tag } && tag != cardFilter)
            {
                cardFilter = tag;
                FillCards();
            }
        };
        tools.Children.Add(filter);
        var right = Horizontal(desktopSwitch, add);
        right.Spacing = 16;
        right.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(right, 1);
        tools.Children.Add(right);
        page.Children.Add(tools);

        cardsStatus = Secondary();
        page.Children.Add(cardsStatus);
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

    private void FillCards()
    {
        if (closed) return;
        cardsPending = false;
        cardsPanel.Children.Clear();
        var cards = app.Cards.Cards
            .Where(c => cardFilter switch
            {
                "archived" => c["archived"].Bool(),
                "done" => !c["archived"].Bool() && c["done"].Bool(),
                _ => !c["archived"].Bool() && !c["done"].Bool(),
            })
            .OrderByDescending(c => c["createdMs"].Num())
            .ToList();
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
        void Patch(JsonObject patch) => _ = PatchCard(id, patch);

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
        var text = new TextBox
        {
            Text = card["text"].Str(),
            AcceptsReturn = true,
            TextWrapping = TextWrapping.Wrap,
            MaxHeight = 240,
        };
        AutomationProperties.SetName(text, T("card.text"));
        text.LostFocus += (_, _) =>
        {
            var value = text.Text.Trim();
            if (value.Length == 0)
            {
                text.Text = card["text"].Str();
            }
            else if (value != card["text"].Str())
            {
                Patch(new JsonObject { ["text"] = value });
            }
            if (cardsPending)
            {
                owner.DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
                {
                    if (!closed && cardsPending && !Editing())
                    {
                        FillCards();
                    }
                });
            }
        };
        stack.Children.Add(text);
        var meta = new List<string> { T(todo ? "cards.kind.todo" : "cards.kind.note"), Format.When(card["created"].Str()) };
        if (!archived && app.Cards.Shown && card["visible"].Bool())
        {
            meta.Add(T("cards.onDesktop"));
        }
        stack.Children.Add(Secondary(string.Join(" · ", meta)));
        Grid.SetColumn(stack, 1);
        grid.Children.Add(stack);

        var menu = new MenuFlyout();
        void Item(string label, string glyph, Action run)
        {
            var item = new MenuFlyoutItem { Text = label, Icon = new FontIcon { Glyph = glyph } };
            item.Click += (_, _) => run();
            menu.Items.Add(item);
        }
        if (!archived)
        {
            var visible = card["visible"].Bool();
            Item(T(visible ? "cards.hideDesktop" : "cards.showDesktop"), visible ? "" : "", () =>
            {
                Patch(new JsonObject { ["visible"] = !visible });
                if (!visible && !app.Cards.Shown)
                {
                    _ = app.Engine.CallAsync("cards.show", new JsonObject { ["on"] = true });
                }
            });
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
        Item(T("card.menu.date"), "", () =>
            owner.DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () => { if (!closed) when.ShowAt(text); }));
        menu.Items.Add(new MenuFlyoutSeparator());
        Item(T("cards.delete"), "", () => _ = DeleteCard(id));
        var more = new Button
        {
            Content = new FontIcon { Glyph = "", FontSize = 14 },
            Flyout = menu,
            VerticalAlignment = VerticalAlignment.Top,
        };
        AutomationProperties.SetName(more, T("cards.more"));
        ToolTipService.SetToolTip(more, T("cards.more"));
        Grid.SetColumn(more, 2);
        grid.Children.Add(more);
        return new Border { Style = Res<Style>("CardStyle"), Child = grid };
    }

    private async Task PatchCard(ulong id, JsonObject patch)
    {
        var reply = await app.Engine.CallAsync("cards.update", new JsonObject { ["id"] = id, ["patch"] = patch });
        if (closed) return;
        cardsStatus.Text = reply.Ok() ? "" : Loc.Error(reply["error"].Str());
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
        var reply = await app.Engine.CallAsync("cards.delete", new JsonObject { ["id"] = id });
        if (closed) return;
        cardsStatus.Text = reply.Ok() ? "" : Loc.Error(reply["error"].Str());
    }
}
