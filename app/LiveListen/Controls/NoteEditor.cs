using System.Text;
using LiveListen.Services;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Input;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.System;
using Windows.UI;
using FormatEffect = Microsoft.UI.Text.FormatEffect;
using TextGetOptions = Microsoft.UI.Text.TextGetOptions;
using TextSetOptions = Microsoft.UI.Text.TextSetOptions;
using UnderlineType = Microsoft.UI.Text.UnderlineType;
using DispatcherQueuePriority = Microsoft.UI.Dispatching.DispatcherQueuePriority;

namespace LiveListen.Controls;

/// <summary>A note surface with native RTF editing and a compact, editing-only toolbar.</summary>
internal sealed class NoteEditor
{
    private const float ListIndent = 12;
    private const int MaxRtfLength = 256 * 1024;
    private NoteTextBox editor;
    private readonly Grid textHost = new();
    private readonly Grid tools = new();
    private readonly Border toolbar = new()
    {
        BorderThickness = new Thickness(0, 1, 0, 0),
        Padding = new Thickness(0, 4, 0, 0),
        Margin = new Thickness(0, 5, 0, 0),
        Visibility = Visibility.Collapsed,
    };
    private readonly TextBlock error = new()
    {
        FontSize = 11,
        TextWrapping = TextWrapping.Wrap,
        Visibility = Visibility.Collapsed,
        Margin = new Thickness(0, 4, 0, 0),
    };
    private readonly SolidColorBrush clear = new(Palette.Transparent);
    private readonly SolidColorBrush foreground = new();
    private readonly SolidColorBrush secondary = new();
    private readonly SolidColorBrush activeToolBrush = new();
    private readonly List<Button> buttons = [];
    private readonly Dictionary<string, Button> toggles = [];
    private SurfaceLook look;
    private readonly bool requireContentPress;
    private bool loading;
    private bool paused;
    private bool popupOpen;
    private bool contextPopupOpen;
    private bool returnFocusAfterFlyout;
    private FlyoutBase? openFlyout;
    private FlyoutBase? contextFlyout;
    private readonly HashSet<FlyoutBase> trackedExternalFlyouts = [];
    private readonly HashSet<FlyoutBase> openExternalFlyouts = [];
    private readonly HashSet<FlyoutBase> borrowedExternalFlyouts = [];
    private bool externalFlyoutsAttached = true;
    private int selectionStart;
    private int selectionEnd;
    private bool restoringSelection;
    private long focusCheckRevision;
    private string? loadedSourceText;
    private string? loadedSourceRtf;
    private (string Text, string Rtf)? loadedDocument;

    public NoteEditor() : this(requireContentPress: false) { }

    public NoteEditor(bool requireContentPress)
    {
        this.requireContentPress = requireContentPress;
        if (requireContentPress) toolbar.BorderThickness = new Thickness(0);
        editor = CreateTextBox();
        View.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        View.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        View.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        textHost.Children.Add(editor);
        View.Children.Add(textHost);
        toolbar.Child = tools;
        Grid.SetRow(toolbar, 1);
        View.Children.Add(toolbar);
        Grid.SetRow(error, 2);
        View.Children.Add(error);

        View.LostFocus += (_, _) => QueueEndIfFocusOutside();
        View.Loaded += (_, _) => AttachExternalFlyouts();
        View.Unloaded += (_, _) =>
        {
            DetachContextFlyout();
            DetachExternalFlyouts();
        };
    }

    private NoteTextBox CreateTextBox()
    {
        var control = new NoteTextBox()
        {
            AcceptsReturn = true,
            TextWrapping = TextWrapping.Wrap,
            // Focus controls the editing session; changing read-only on every focus/selection
            // transition makes native RichEdit recreate its caret and flash the selection.
            IsReadOnly = requireContentPress && !IsEditing,
            SelectionFlyout = null,
            BorderThickness = new Thickness(0),
            Padding = new Thickness(0, 2, 0, 2),
            MinHeight = 24,
            MaxHeight = 320,
            FontSize = 14,
            IsSpellCheckEnabled = false,
        };
        foreach (var key in new[] { "TextControlBackground", "TextControlBackgroundPointerOver", "TextControlBackgroundFocused",
                     "TextControlBorderBrush", "TextControlBorderBrushPointerOver", "TextControlBorderBrushFocused" })
            control.Resources[key] = clear;
        foreach (var key in new[] { "TextControlForeground", "TextControlForegroundPointerOver", "TextControlForegroundFocused" })
            control.Resources[key] = foreground;
        control.Resources["TextControlPlaceholderForeground"] = secondary;
        control.Resources["TextControlPlaceholderForegroundFocused"] = secondary;
        control.Resources["TextControlBorderThemeThicknessFocused"] = new Thickness(0);
        control.PlaceholderText = Loc.T("card.placeholder");
        AutomationProperties.SetName(control, Loc.T("card.text"));
        ToolTipService.SetToolTip(control, Loc.T("format.editHint"));

        control.Foreground = new SolidColorBrush(look.Text);
        control.TextChanged += (_, _) =>
        {
            if (control == editor) EditorTextChanged();
        };
        control.SelectionChanged += (_, _) =>
        {
            if (control == editor) EditorSelectionChanged();
        };
        if (requireContentPress)
        {
            control.AddHandler(UIElement.PointerPressedEvent, new PointerEventHandler((_, e) =>
            {
                if (control == editor && (e.Pointer.PointerDeviceType != PointerDeviceType.Mouse
                    || e.GetCurrentPoint(control).Properties.IsLeftButtonPressed))
                    EnterEditing(requestFocus: false);
            }), true);
        }
        else control.GotFocus += EditorGotFocus;
        control.AddHandler(UIElement.DoubleTappedEvent,
            new DoubleTappedEventHandler((_, e) => { BeginEditing(); e.Handled = true; }), true);
        control.PreviewKeyDown += EditorKeyDown;
        control.Loaded += (_, _) => { if (control == editor) ObserveContextFlyout(); };
        control.LostFocus += (_, _) => QueueEndIfFocusOutside();
        return control;
    }

    public Grid View { get; } = new();
    public bool IsEditing { get; private set; }
    public bool IsEmpty => PlainText().Length == 0;
    public bool HasOpenFlyout => popupOpen || contextPopupOpen || openExternalFlyouts.Count > 0;

    /// <summary>Keeps the editor stable while a containing card's menu takes focus.</summary>
    public void TrackExternalFlyout(FlyoutBase flyout, bool isOpening = false)
    {
        // A containing list can attach the currently active editor from the
        // flyout's Opening event, after that event's invocation list was captured.
        if (isOpening) openExternalFlyouts.Add(flyout);
        if (!trackedExternalFlyouts.Add(flyout)) return;
        if (isOpening) borrowedExternalFlyouts.Add(flyout);
        if (externalFlyoutsAttached)
        {
            flyout.Opening += ExternalFlyoutOpening;
            flyout.Closed += ExternalFlyoutClosed;
        }
    }

    private void ExternalFlyoutOpening(object? sender, object args)
    {
        if (sender is FlyoutBase flyout) openExternalFlyouts.Add(flyout);
    }

    private void ExternalFlyoutClosed(object? sender, object args)
    {
        if (sender is FlyoutBase flyout)
        {
            openExternalFlyouts.Remove(flyout);
            if (borrowedExternalFlyouts.Remove(flyout))
            {
                trackedExternalFlyouts.Remove(flyout);
                flyout.Opening -= ExternalFlyoutOpening;
                flyout.Closed -= ExternalFlyoutClosed;
            }
        }
        QueueEndIfFocusOutside();
    }

    private void AttachExternalFlyouts()
    {
        if (externalFlyoutsAttached) return;
        externalFlyoutsAttached = true;
        foreach (var flyout in trackedExternalFlyouts)
        {
            flyout.Opening += ExternalFlyoutOpening;
            flyout.Closed += ExternalFlyoutClosed;
        }
    }

    private void DetachExternalFlyouts()
    {
        if (!externalFlyoutsAttached) return;
        externalFlyoutsAttached = false;
        foreach (var flyout in trackedExternalFlyouts)
        {
            flyout.Opening -= ExternalFlyoutOpening;
            flyout.Closed -= ExternalFlyoutClosed;
        }
        trackedExternalFlyouts.ExceptWith(borrowedExternalFlyouts);
        borrowedExternalFlyouts.Clear();
        openExternalFlyouts.Clear();
    }
    public bool CanSave => TryGetContent(out _);

    public bool TryGetContent(out (string Text, string Rtf) content)
    {
        content = GetContent();
        return content.Text.EnumerateRunes().Count() <= 4000 && Encoding.UTF8.GetByteCount(content.Rtf) <= MaxRtfLength
            && !HasEmbeddedContent(content.Rtf);
    }
    public event Action? Changed;
    public event Action? EditingEnded;

    public void Load(string text, string? rtf)
    {
        var normalized = Normalize(text);
        var normalizedRtf = rtf?.TrimEnd('\0');
        if (loadedDocument is { } previous)
        {
            var current = GetContent();
            // cards.changed can replay the same snapshot while the native editor still
            // has a selection. Never SetText for an unchanged document: it resets undo,
            // caret and the insertion format even when the visible text is identical.
            if ((normalized == current.Text && normalizedRtf == current.Rtf)
                || (normalized == loadedSourceText && normalizedRtf == loadedSourceRtf && current == previous))
                return;
        }
        loading = true;
        var previousEditor = editor;
        var wasReadOnly = previousEditor.IsReadOnly;
        // Whole-document RTF loads can reset the native foreground of a live
        // RichEditBox. Prepare the new document before attaching it to the view.
        // The unchanged-document checks above keep the existing editor and undo.
        if (previousEditor.IsLoaded) editor = CreateTextBox();
        editor.IsReadOnly = false;
        try
        {
            var accepted = false;
            if (!string.IsNullOrEmpty(rtf) && Encoding.UTF8.GetByteCount(rtf) <= MaxRtfLength && rtf.StartsWith("{\\rtf1", StringComparison.Ordinal)
                && !HasEmbeddedContent(rtf))
            {
                try
                {
                    // Open a whole RTF document rather than insert a fragment. The
                    // fragment path keeps the current mandatory EOP as an extra
                    // paragraph on every reload; document defaults preserve it.
                    editor.Document.SetText(TextSetOptions.FormatRtf | TextSetOptions.ApplyRtfDocumentDefaults, rtf);
                    accepted = PlainText() == normalized;
                }
                catch (Exception) { /* Invalid RTF falls back to the stored plain text. */ }
            }
            if (!accepted)
            {
                editor.Document.SetText(TextSetOptions.None, text.Replace("\r\n", "\n").Replace('\n', '\r'));
                var entire = editor.Document.GetRange(0, editor.Document.Selection.StoryLength);
                var format = entire.CharacterFormat;
                format.ForegroundColor = TextConstants.AutoColor;
                entire.CharacterFormat = format;
            }
            else NormalizeDefaultColors();
            selectionStart = selectionEnd = 0;
            editor.Document.Selection.SetRange(0, 0);
            error.Visibility = Visibility.Collapsed;
            loadedSourceText = normalized;
            loadedSourceRtf = normalizedRtf;
            loadedDocument = GetContent();
        }
        finally
        {
            editor.IsReadOnly = wasReadOnly;
            if (editor != previousEditor)
            {
                DetachContextFlyout();
                var restoreFocus = previousEditor.FocusState != FocusState.Unfocused;
                textHost.Children[0] = editor;
                if (restoreFocus) FocusEditorIfNeeded();
            }
            loading = false;
        }
    }

    public (string Text, string Rtf) GetContent()
    {
        editor.Document.GetText(TextGetOptions.FormatRtf, out var rtf);
        return (PlainText(), rtf.TrimEnd('\0'));
    }

    public void BeginEditing() => EnterEditing(requestFocus: true);

    private void EnterEditing(bool requestFocus)
    {
        if (paused || loading) return;
        if (!IsEditing)
        {
            IsEditing = true;
            if (requireContentPress) editor.IsReadOnly = false;
            EnsureToolbar();
            toolbar.Visibility = Visibility.Visible;
            RefreshToggles();
            Changed?.Invoke();
        }
        if (requestFocus) FocusEditorIfNeeded();
    }

    public void FinishEditing()
    {
        if (!IsEditing) return;
        if (!CanSave)
        {
            CheckLength();
            return;
        }
        IsEditing = false;
        if (requireContentPress) editor.IsReadOnly = true;
        openFlyout?.Hide();
        toolbar.Visibility = Visibility.Collapsed;
        EditingEnded?.Invoke();
        Changed?.Invoke();
    }

    public void Pause()
    {
        paused = true;
        FinishEditing();
        editor.IsReadOnly = true;
        View.IsHitTestVisible = false;
    }

    public void Resume()
    {
        paused = false;
        editor.IsReadOnly = requireContentPress && !IsEditing;
        View.IsHitTestVisible = true;
    }

    public void Apply(SurfaceLook value)
    {
        if (look == value) return;
        // Keep native document formatting out of appearance refreshes. Applying
        // AutoColor to a live range resets it to black until the host color changes.
        if (look.Text != value.Text)
        {
            foreground.Color = value.Text;
            editor.Foreground = new SolidColorBrush(value.Text);
        }
        look = value;
        secondary.Color = value.Secondary;
        activeToolBrush.Color = value.Hover;
        toolbar.Background = clear;
        if (!requireContentPress) toolbar.BorderBrush = new SolidColorBrush(Theme.WithAlpha(value.Text, 30));
        error.Foreground = new SolidColorBrush(value.Dark ? Color.FromArgb(255, 255, 145, 145) : Color.FromArgb(255, 175, 35, 35));
        foreach (var button in buttons)
            ApplyToolStyle(button);
        RefreshToggles();
    }

    private void EnsureToolbar()
    {
        if (buttons.Count != 0) return;
        AddTool("bold", Letter("B", bold: true), () => Character(f => f.Bold = FormatEffect.Toggle));
        AddTool("italic", Letter("I", italic: true), () => Character(f => f.Italic = FormatEffect.Toggle));
        AddTool("underline", Letter("U", line: 21), () => Character(f =>
            f.Underline = f.Underline == UnderlineType.None ? UnderlineType.Single : UnderlineType.None));
        AddTool("strike", Letter("S", line: 12), () => Character(f => f.Strikethrough = FormatEffect.Toggle));
        AddTool("link", Icon("\uE71B"), ShowLink, takesFocus: true);
        AddTool("list", NoteListIcons.Create("bullet", foreground), ShowLists);
        foreach (var button in buttons) ApplyToolStyle(button);
    }

    private void ApplyToolStyle(Button button)
    {
        button.Foreground = foreground;
        button.Resources["ButtonBackgroundPointerOver"] = activeToolBrush;
        button.Resources["ButtonBackgroundPressed"] = new SolidColorBrush(look.Pressed);
        button.Resources["ButtonForegroundPointerOver"] = foreground;
        button.Resources["ButtonForegroundPressed"] = foreground;
    }

    private static string Normalize(string text) => text.Replace("\r\n", "\n").Replace('\r', '\n').Trim();

    private void NormalizeDefaultColors()
    {
        // RichEdit can save the card's soft black/white default as explicit RGB.
        // Restore AutoColor so a change of background can update that default.
        var document = editor.Document;
        var length = document.Selection.StoryLength;
        var start = 0;
        while (start < length)
        {
            var range = document.GetRange(start, Math.Min(length, start + 1));
            range.Expand(TextRangeUnit.CharacterFormat);
            var next = Math.Max(start + 1, range.EndPosition);
            var format = range.CharacterFormat;
            var color = format.ForegroundColor;
            if (color.A == 255 && color.R == color.G && color.G == color.B && color.R is 36 or 245)
            {
                format.ForegroundColor = TextConstants.AutoColor;
                range.CharacterFormat = format;
            }
            start = next;
        }
    }

    private string PlainText()
    {
        editor.Document.GetText(TextGetOptions.NoHidden, out var text);
        return Normalize(text);
    }

    private void CheckLength()
    {
        var content = GetContent();
        var tooLong = content.Text.EnumerateRunes().Count() > 4000;
        var formatTooLong = Encoding.UTF8.GetByteCount(content.Rtf) > MaxRtfLength;
        var unsupported = HasEmbeddedContent(content.Rtf);
        error.Text = tooLong ? Loc.T("error.text_too_long") : formatTooLong ? Loc.T("error.card_format_too_large")
            : unsupported ? Loc.T("error.bad_rich_text") : "";
        error.Visibility = tooLong || formatTooLong || unsupported ? Visibility.Visible : Visibility.Collapsed;
    }

    private static bool HasEmbeddedContent(string rtf)
    {
        if (rtf.Contains('\0')) return true;
        for (var i = 0; i < rtf.Length; i++)
        {
            if (rtf[i] != '\\' || ++i >= rtf.Length) continue;
            var start = i;
            while (i < rtf.Length && char.IsAsciiLetter(rtf[i])) i++;
            if (start == i) continue; // Escaped backslashes/braces are literal text.
            var word = rtf[start..i].ToLowerInvariant();
            if (word is "object" or "objdata" or "objclass" or "objname" or "objalias" or "oleclsid"
                or "pict" or "shppict" or "nonshppict" or "listpicture" or "bin" or "filetbl" or "file" or "datastore")
                return true;
            i--;
        }
        return false;
    }

    private FrameworkElement Letter(string text, bool bold = false, bool italic = false, double? line = null)
    {
        var grid = new Grid { Width = 24, Height = 25 };
        grid.Children.Add(new TextBlock
        {
            Text = text,
            FontSize = 20,
            FontWeight = bold ? FontWeights.Bold : FontWeights.Normal,
            FontStyle = italic ? Windows.UI.Text.FontStyle.Italic : Windows.UI.Text.FontStyle.Normal,
            FontFamily = new FontFamily(italic ? "Georgia" : "Segoe UI"),
            Foreground = foreground,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
        });
        if (line is double y)
            grid.Children.Add(new Line { X1 = 5, X2 = 20, Y1 = y, Y2 = y, Stroke = foreground, StrokeThickness = 1.5 });
        return grid;
    }

    private FontIcon Icon(string glyph) => new() { Glyph = glyph, FontSize = 18, Foreground = foreground };

    private void AddTool(string name, FrameworkElement icon, Action run, bool takesFocus = false)
    {
        var button = new Button
        {
            Content = icon,
            MinWidth = 0,
            MinHeight = 0,
            Height = 30,
            Padding = new Thickness(0),
            BorderThickness = new Thickness(0),
            CornerRadius = new CornerRadius(12),
            Background = clear,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            // A form flyout must have a focusable anchor; formatting-only buttons
            // keep the native selection focus so they can apply repeatedly.
            AllowFocusOnInteraction = takesFocus,
        };
        AutomationProperties.SetName(button, Loc.T("format." + name));
        ToolTipService.SetToolTip(button, Loc.T("format." + name));
        Grid.SetColumn(button, tools.ColumnDefinitions.Count);
        tools.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        tools.Children.Add(button);
        buttons.Add(button);
        toggles[name] = button;
        button.Click += (_, _) =>
        {
            if (!IsEditing) return;
            RestoreSelection();
            run();
        };
    }

    private void RestoreSelection()
    {
        if (editor.Document.Selection.StartPosition == selectionStart && editor.Document.Selection.EndPosition == selectionEnd) return;
        restoringSelection = true;
        try { editor.Document.Selection.SetRange(selectionStart, selectionEnd); }
        finally { restoringSelection = false; }
    }

    private void Character(Action<ITextCharacterFormat> change)
    {
        var range = editor.Document.Selection;
        var format = range.CharacterFormat;
        change(format);
        range.CharacterFormat = format;
        FormattingChanged();
    }

    private void FormattingChanged()
    {
        selectionStart = editor.Document.Selection.StartPosition;
        selectionEnd = editor.Document.Selection.EndPosition;
        RefreshToggles();
        Changed?.Invoke();
        if (popupOpen) returnFocusAfterFlyout = true;
        else if (IsEditing && FocusIsWithinEditor()) FocusEditorIfNeeded();
    }

    private void EditorSelectionChanged()
    {
        if (loading || restoringSelection || HasOpenFlyout) return;
        var selected = editor.Document.Selection;
        var changed = selectionStart != selected.StartPosition || selectionEnd != selected.EndPosition;
        selectionStart = selected.StartPosition;
        selectionEnd = selected.EndPosition;
        if (!requireContentPress && changed && selectionStart != selectionEnd && editor.FocusState != FocusState.Unfocused)
            EnterEditing(requestFocus: false);
        RefreshToggles();
    }

    private void EditorTextChanged()
    {
        if (loading || requireContentPress && !IsEditing) return;
        // Visible RichEdit also raises delayed format/load notifications. Only a real
        // text focus (including continued typing after Esc) can start a new session.
        if (IsEditing || editor.FocusState != FocusState.Unfocused) EnterEditing(requestFocus: false);
        CheckLength();
        Changed?.Invoke();
    }

    private void EditorGotFocus(object sender, RoutedEventArgs e) => EnterEditing(requestFocus: false);

    private void FocusEditorIfNeeded()
    {
        if (!paused && !HasOpenFlyout && editor.FocusState == FocusState.Unfocused && View.XamlRoot is not null)
            editor.Focus(FocusState.Programmatic);
    }

    private bool FocusIsWithinEditor()
    {
        if (View.XamlRoot is null) return false;
        var focused = FocusManager.GetFocusedElement(View.XamlRoot) as DependencyObject;
        while (focused is not null)
        {
            if (focused == View) return true;
            focused = VisualTreeHelper.GetParent(focused);
        }
        return false;
    }

    private void QueueEndIfFocusOutside()
    {
        if (!IsEditing) return;
        var revision = ++focusCheckRevision;
        View.DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () =>
        {
            if (revision != focusCheckRevision || !IsEditing || HasOpenFlyout || View.XamlRoot is null) return;
            if (!FocusIsWithinEditor()) FinishEditing();
        });
    }

    private void ObserveContextFlyout()
    {
        if (editor.ContextFlyout is not { } flyout || flyout == contextFlyout) return;
        DetachContextFlyout();
        contextFlyout = flyout;
        flyout.Opening += ContextFlyoutOpening;
        flyout.Closed += ContextFlyoutClosed;
    }

    private void DetachContextFlyout()
    {
        if (contextFlyout is null) return;
        contextFlyout.Opening -= ContextFlyoutOpening;
        contextFlyout.Closed -= ContextFlyoutClosed;
        contextFlyout = null;
        contextPopupOpen = false;
    }

    private void ContextFlyoutOpening(object? sender, object args) => contextPopupOpen = true;

    private void ContextFlyoutClosed(object? sender, object args)
    {
        contextPopupOpen = false;
        QueueEndIfFocusOutside();
    }

    private void RefreshToggles()
    {
        if (!IsEditing) return;
        var f = editor.Document.Selection.CharacterFormat;
        void Set(string key, bool active) => toggles[key].Background = active ? activeToolBrush : clear;
        Set("bold", f.Bold == FormatEffect.On);
        Set("italic", f.Italic == FormatEffect.On);
        Set("underline", f.Underline is not UnderlineType.None and not UnderlineType.Undefined);
        Set("strike", f.Strikethrough == FormatEffect.On);
    }

    private void ShowMenu(MenuFlyout menu, FrameworkElement anchor)
    {
        menu.ShouldConstrainToRootBounds = false;
        menu.Placement = FlyoutPlacementMode.Top;
        RegisterFlyout(menu);
        menu.ShowAt(anchor);
    }

    private void RegisterFlyout(FlyoutBase flyout)
    {
        popupOpen = true;
        returnFocusAfterFlyout = false;
        openFlyout = flyout;
        flyout.Closed += (_, _) =>
        {
            var restoreFocus = returnFocusAfterFlyout;
            popupOpen = false;
            returnFocusAfterFlyout = false;
            openFlyout = null;
            if (IsEditing && restoreFocus)
            {
                // The flyout restores its anchor after Closed. Restore the text
                // focus on the next dispatch turn, after that native restoration.
                ++focusCheckRevision;
                View.DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () =>
                {
                    if (IsEditing && !HasOpenFlyout && FocusIsWithinEditor()) FocusEditorIfNeeded();
                    else QueueEndIfFocusOutside();
                });
            }
            else QueueEndIfFocusOutside();
        };
    }

    private ITextRange LinkRange()
    {
        var selection = editor.Document.Selection;
        var range = editor.Document.GetRange(selection.StartPosition, selection.EndPosition);
        // Link requires a nonempty range. Probe the character at an insertion
        // point so editing/removing an existing friendly-name link also works.
        if (range.Length == 0 && range.StartPosition < selection.StoryLength - 1)
        {
            var probe = editor.Document.GetRange(range.StartPosition, range.StartPosition + 1);
            if (probe.Link.Trim('\uFDDF', '"').Length > 0) range = probe;
        }
        if (range.Link.Trim('\uFDDF', '"').Length > 0) range.Expand(TextRangeUnit.Link);
        // Selecting to the end of a visual line can include its paragraph mark.
        // It stays in the document, outside the hyperlink's friendly name.
        while (range.Length > 0 && editor.Document.GetRange(range.EndPosition - 1, range.EndPosition).Character is '\r' or '\n')
            range.EndPosition--;
        return range;
    }

    private void ShowLink()
    {
        var flyout = BuildLinkFlyout();
        RegisterFlyout(flyout);
        flyout.ShowAt(toggles["link"]);
    }

    private Flyout BuildLinkFlyout()
    {
        // Keep an independent native range while the popup's two TextBoxes own
        // keyboard focus. Their selection must never replace the document range.
        var range = LinkRange();
        var field = new TextBox { Header = Loc.T("format.link.address"), PlaceholderText = "https://", Width = 232 };
        var existing = range.Link.Trim('\uFDDF', '"');
        if (existing.Length > 0) field.Text = existing;
        range.GetText(TextGetOptions.NoHidden, out var selectedText);
        var initialLabel = selectedText.Replace("\r", "").Replace("\n", "");
        var label = new TextBox { Header = Loc.T("format.link.text"), Text = initialLabel, Width = 232 };
        var validation = new TextBlock { Foreground = error.Foreground, TextWrapping = TextWrapping.Wrap, MaxWidth = 232, Visibility = Visibility.Collapsed };
        var apply = new Button { Content = Loc.T("format.link.apply") };
        var remove = new Button { Content = Loc.T("format.link.remove"), IsEnabled = existing.Length > 0 };
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        row.Children.Add(apply);
        row.Children.Add(remove);
        var panel = new StackPanel { Spacing = 10 };
        panel.Children.Add(field);
        panel.Children.Add(label);
        panel.Children.Add(validation);
        panel.Children.Add(row);
        var flyout = new Flyout
        {
            Content = panel, ShouldConstrainToRootBounds = false, Placement = FlyoutPlacementMode.Top,
            AllowFocusOnInteraction = true, ShowMode = FlyoutShowMode.Standard,
        };
        flyout.Opened += (_, _) => { field.Focus(FocusState.Programmatic); field.SelectAll(); };
        apply.Click += (_, _) =>
        {
            // A single-line TextBox normalizes paragraph breaks. Its initial display
            // is not a user edit and must never be written back over rich text.
            if (!ApplyLink(range, field.Text, label.Text == initialLabel ? null : label.Text))
            {
                validation.Text = Loc.T("format.link.invalid");
                validation.Visibility = Visibility.Visible;
                return;
            }
            flyout.Hide();
        };
        remove.Click += (_, _) =>
        {
            RemoveLink(range);
            flyout.Hide();
        };
        return flyout;
    }

    private bool ApplyLink(ITextRange range, string address, string? label)
    {
        var url = address.Trim();
        if (!Uri.TryCreate(url, UriKind.Absolute, out var uri) || uri.Scheme is not ("http" or "https" or "mailto")
            || url.IndexOfAny(['"', '\r', '\n', '\0']) >= 0) return false;
        range.GetText(TextGetOptions.NoHidden, out var originalText);
        var text = !string.IsNullOrEmpty(label) ? label : originalText.Length > 0 ? originalText : url;
        if (text != originalText)
        {
            if (range.Link.Trim('\uFDDF', '"').Length > 0) range.Link = "";
            var start = range.StartPosition;
            range.SetText(TextSetOptions.None, text);
            range.SetRange(start, start + text.Length);
        }
        range.Link = '"' + uri.AbsoluteUri + '"';
        editor.Document.Selection.SetRange(range.EndPosition, range.EndPosition);
        FormattingChanged();
        return true;
    }

    private void RemoveLink(ITextRange range)
    {
        if (range.Link.Trim('\uFDDF', '"').Length == 0) return;
        range.Expand(TextRangeUnit.Link);
        range.Link = "";
        editor.Document.Selection.SetRange(range.EndPosition, range.EndPosition);
        FormattingChanged();
    }

    private void ShowLists() => ShowMenu(BuildListMenu(), toggles["list"]);

    private MenuFlyout BuildListMenu()
    {
        var menu = new MenuFlyout();
        foreach (var key in new[] { "bullet", "dash", "number" })
        {
            var item = new MenuFlyoutItem { Text = Loc.T("format.list." + key), Icon = NoteListIcons.Create(key) };
            item.Click += (_, _) => { RestoreSelection(); SetList(key); };
            menu.Items.Add(item);
        }
        menu.Items.Add(new MenuFlyoutSeparator());
        var none = new MenuFlyoutItem { Text = Loc.T("format.list.none") };
        none.Click += (_, _) => { RestoreSelection(); SetList("none"); };
        menu.Items.Add(none);
        return menu;
    }

    private void SetList(string kind)
    {
        if (kind is not ("bullet" or "dash" or "number" or "none")) return;
        var selection = editor.Document.Selection;
        var start = selection.StartPosition;
        var end = selection.EndPosition;
        var paragraph = editor.Document.GetRange(start, end);
        paragraph.Expand(TextRangeUnit.Paragraph);
        var paragraphStart = paragraph.StartPosition;
        var paragraphEnd = paragraph.EndPosition;
        // Work backwards so inserting or removing a marker never shifts an earlier range.
        var lines = Paragraphs().Where(p => p.Start < paragraphEnd && p.End > paragraphStart).Reverse().ToArray();
        var delta = 0;
        foreach (var line in lines)
        {
            var prefix = line.Text.StartsWith("– ", StringComparison.Ordinal) ? 2 : 0;
            if (prefix > 0) editor.Document.GetRange(line.Start, line.Start + prefix).Text = "";
            var marker = kind == "dash" ? "– " : "";
            if (marker.Length > 0) editor.Document.GetRange(line.Start, line.Start).Text = marker;
            delta += marker.Length - prefix;
        }
        var finalEnd = Math.Max(paragraphStart, Math.Min(selection.StoryLength - 1, paragraphEnd + delta));
        var range = editor.Document.GetRange(paragraphStart, finalEnd);
        var format = range.ParagraphFormat;
        format.ListType = kind == "bullet" ? MarkerType.Bullet : kind == "number" ? MarkerType.Arabic : MarkerType.None;
        if (kind is "bullet" or "number")
        {
            format.ListStyle = kind == "number" ? MarkerStyle.Period : MarkerStyle.Plain;
            format.ListStart = 1;
            format.SetIndents(-ListIndent, ListIndent, 0);
        }
        else format.SetIndents(kind == "dash" ? -ListIndent : 0, kind == "dash" ? ListIndent : 0, 0);
        range.ParagraphFormat = format;
        selection.SetRange(paragraphStart, finalEnd);
        FormattingChanged();
    }

    private void EditorKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (!requireContentPress && !IsEditing && e.Key is VirtualKey.Enter or VirtualKey.F2) { BeginEditing(); e.Handled = true; return; }
        if (!IsEditing) return;
        if (e.Key == VirtualKey.Escape) { FinishEditing(); e.Handled = true; return; }
        if (e.Key != VirtualKey.Enter || editor.Document.Selection.Length != 0) return;
        var selected = editor.Document.Selection;
        var paragraph = editor.Document.GetRange(selected.StartPosition, selected.EndPosition);
        paragraph.Expand(TextRangeUnit.Paragraph);
        paragraph.GetText(TextGetOptions.NoHidden, out var text);
        if (!text.StartsWith("– ", StringComparison.Ordinal)) return;
        if (text[2..].Trim().Length == 0)
        {
            editor.Document.GetRange(paragraph.StartPosition, paragraph.StartPosition + 2).Text = "";
            selected.ParagraphFormat.SetIndents(0, 0, 0);
        }
        else selected.TypeText("\r– ");
        e.Handled = true;
        FormattingChanged();
    }

    private IEnumerable<(int Start, int End, string Text)> Paragraphs()
    {
        // Include the mandatory final paragraph so list commands also work in
        // a new note or in an empty paragraph after the user presses Enter.
        editor.Document.GetText(TextGetOptions.AllowFinalEop, out var text);
        var start = 0;
        for (var i = 0; i < text.Length; i++)
        {
            if (text[i] != '\r') continue;
            yield return (start, i + 1, text[start..i]);
            start = i + 1;
        }
        if (start < text.Length) yield return (start, text.Length, text[start..]);
    }

}
