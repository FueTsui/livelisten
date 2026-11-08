using System.Text.Json.Nodes;
using LiveListen.Services;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace LiveListen.Views;

/// <summary>Card appearance: the style shared by all cards, or one card's own style.</summary>
internal sealed partial class SettingsWindow
{
    private ulong? styleCard
    {
        get => s.Get("appearance.styleCard") is JsonValue value && value.TryGetValue<ulong>(out var id) ? id : null;
        set { if (value != styleCard) Set("appearance.styleCard", value.HasValue ? JsonValue.Create(value.Value) : null); }
    }
    // The latest style edited here for `styleCard` (null: shared), ahead of the engine.
    private (ulong Id, JsonObject? Style, long Revision)? cardStyleLocal;
    private DispatcherQueueTimer? cardStyleTimer;
    private bool cardStylePending;
    private int cardStyleSaving;
    private long cardStyleRevision;
    private long cardStyleCompletedRevision;
    private Task cardStyleWrites = Task.CompletedTask;

    private static ulong CardId(JsonObject card) => (ulong)card["id"].Num();

    private void BuildCardStyle()
    {
        var cards = app.Cards.Cards.Where(c => !c["archived"].Bool()).OrderByDescending(c => c["createdMs"].Num()).ToList();
        if (styleCard is { } chosen && cards.All(c => CardId(c) != chosen))
        {
            styleCard = null;
        }
        var target = new ComboBox { MinWidth = 280 };
        target.Items.Add(new ComboBoxItem { Content = T("look.cards.all"), Tag = "" });
        foreach (var card in cards)
        {
            var text = card["text"].Str().ReplaceLineEndings(" ").Trim();
            var name = text.Length > 24 ? text[..24] + "…" : text;
            if (card["style"] is JsonObject)
            {
                name += " · " + T("look.cards.ownMark");
            }
            target.Items.Add(new ComboBoxItem { Content = name, Tag = CardId(card).ToString() });
        }
        target.SelectedIndex = styleCard is { } id ? 1 + cards.FindIndex(c => CardId(c) == id) : 0;
        target.SelectionChanged += (_, _) =>
        {
            if (target.SelectedItem is ComboBoxItem { Tag: string tag })
            {
                var chosen = tag.Length == 0 ? (ulong?)null : ulong.Parse(tag);
                if (chosen == styleCard) return;
                FlushCardStyle();
                styleCard = chosen;
                ShowPage(current, keepScroll: true);
            }
        };
        pageClosing = FlushCardStyle;
        if (styleCard is { } cardId)
        {
            BuildOneCardStyle(cardId, target);
        }
        else
        {
            BuildSharedCardStyle(target);
        }
    }

    private void BuildSharedCardStyle(ComboBox target)
    {
        BuildStyleEditor("cards", SettingsTarget("cards"), T("look.reset"), () => ResetSurface("cards"), () =>
        {
            Card(T("look.cards.target"), null, target);
            var clear = ActionButton(T("look.cards.clear"), ClearCardStyles);
            Card(T("look.cards.own.title"), "", clear, null, out var count);
            void Count()
            {
                var n = app.Cards.Cards.Count(c => c["style"] is JsonObject);
                count.Text = n == 0 ? "" : Loc.F("look.cards.own.count", n);
                count.Visibility = n == 0 ? Visibility.Collapsed : Visibility.Visible;
                clear.IsEnabled = n > 0;
            }
            Count();
            pageEvents = (name, _) =>
            {
                if (name == "cards.changed")
                {
                    Count();
                }
            };
        });
    }

    private void BuildOneCardStyle(ulong id, ComboBox target)
    {
        JsonObject? Effective() => cardStyleLocal is { } local && local.Id == id ? local.Style : app.Cards.EffectiveStyle(id);
        var own = Effective() is not null;
        // Compare incoming saves with what these controls displayed, not with Saved()
        // read a second time after CardsHost has already replaced its snapshot.
        JsonNode? displayedStyle = Effective()?.DeepClone();
        var editor = new StyleTarget(
            key => (Effective() ?? SharedCardStyle())[key],
            (key, value) =>
            {
                var style = (JsonObject)(Effective() ?? SharedCardStyle()).DeepClone();
                if (JsonNode.DeepEquals(style[key], value)) return;
                style[key] = value?.DeepClone();
                cardStyleLocal = (id, style, ++cardStyleRevision);
                displayedStyle = style.DeepClone();
                app.Cards.PreviewStyle(id, style);
                QueueCardStyle();
                afterOwnChange?.Invoke();
            },
            () => Theme.Look(s, "cards", Effective()));
        BuildStyleEditor("cards", editor, T("look.card.useShared"), () =>
        {
            cardStyleTimer?.Stop();
            cardStylePending = false;
            cardStyleLocal = (id, null, ++cardStyleRevision);
            SaveCardStyle(id, null, cardStyleRevision);
            ShowPage(current, keepScroll: true);
        }, () =>
        {
            Card(T("look.cards.target"), null, target);
            page.Children.Add(Note(T(own ? "look.card.own" : "look.card.shared")));
        });
        pageEvents = (name, _) =>
        {
            // A change made elsewhere, such as from the card's own menu, while nothing is
            // waiting to be saved here: show what the engine has.
            if (name == "cards.changed" && cardStyleSaving == 0 && !cardStylePending
                && !(cardStyleLocal is { } draft && draft.Id == id)
                && !JsonNode.DeepEquals(displayedStyle, Effective()))
            {
                ShowPage(current, keepScroll: true);
            }
        };
    }

    private JsonObject SharedCardStyle() => s.Get("appearance.cards") as JsonObject ?? new JsonObject();

    private void QueueCardStyle()
    {
        cardStylePending = true;
        if (cardStyleTimer is null)
        {
            cardStyleTimer = DispatcherQueue.CreateTimer();
            cardStyleTimer.Interval = TimeSpan.FromMilliseconds(300);
            cardStyleTimer.IsRepeating = false;
            cardStyleTimer.Tick += (_, _) => FlushCardStyle();
        }
        cardStyleTimer.Stop();
        cardStyleTimer.Start();
    }

    /// <summary>Saves a card style that is still waiting for the editing pause.</summary>
    private void FlushCardStyle()
    {
        if (!cardStylePending)
        {
            return;
        }
        cardStyleTimer?.Stop();
        cardStylePending = false;
        if (cardStyleLocal is { } local)
        {
            SaveCardStyle(local.Id, local.Style, local.Revision);
        }
    }

    private void SaveCardStyle(ulong id, JsonObject? style, long revision)
    {
        cardStyleSaving++;
        cardStyleWrites = SaveCardStyleAsync(id, style?.DeepClone() as JsonObject, revision);
        app.Cards.Track(cardStyleWrites);
    }

    private async Task SaveCardStyleAsync(ulong id, JsonObject? style, long revision)
    {
        try
        {
            await app.Cards.SetStyle(id, style);
            cardStyleCompletedRevision = Math.Max(cardStyleCompletedRevision, revision);
        }
        catch (Exception)
        {
            ShowIssue(Loc.Error("save_failed"));
        }
        finally
        {
            cardStyleSaving--;
            FinishCardStyleWrite();
        }
    }

    private void ClearCardStyles()
    {
        FlushCardStyle();
        cardStyleSaving++;
        cardStyleWrites = ClearCardStylesAsync(cardStyleRevision);
        app.Cards.Track(cardStyleWrites);
    }

    private async Task ClearCardStylesAsync(long revision)
    {
        try
        {
            var reply = await app.Cards.ClearStyles();
            if (!reply.Ok()) ShowIssue(Loc.Error(reply["error"].Str()));
            else if (cardStyleLocal is { } local && local.Revision <= revision) cardStyleLocal = null;
        }
        catch (Exception)
        {
            ShowIssue(Loc.Error("save_failed"));
        }
        finally
        {
            cardStyleSaving--;
            FinishCardStyleWrite();
        }
    }

    private void RetireSavedCardStyle()
    {
        // CardsHost owns every submitted snapshot, including failed writes, across
        // settings windows. Keep only an edit still waiting for the debounce here.
        if (cardStylePending || cardStyleSaving != 0 || cardStyleLocal is not { } local
            || local.Revision > cardStyleCompletedRevision) return;
        cardStyleLocal = null;
    }

    private void FinishCardStyleWrite()
    {
        RetireSavedCardStyle();
        // Recheck the currently displayed card after every queued write settles.
        // Its event may have arrived while another card was still being saved.
        if (cardStyleSaving == 0 && !cardStylePending) pageEvents?.Invoke("cards.changed", new JsonObject());
    }
}
