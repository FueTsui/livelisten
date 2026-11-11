using System.Text.Json.Nodes;
using LiveListen.Services;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;
using Windows.UI;

namespace LiveListen.Views;

/// <summary>The settings window: Mica, a navigation pane and pages of settings cards.</summary>
internal sealed partial class SettingsWindow : Window
{
    private static readonly (string Tag, string Glyph)[] Pages =
    [
        ("dictation", ""),
        ("captions", ""),
        ("notebook", ""),
        ("shortcuts", ""),
        ("appearance", ""),
        ("services", ""),
        ("general", ""),
    ];

    private readonly App app;
    private readonly SettingsStore s;
    private readonly Grid root = new();
    private readonly NavigationView nav = new()
    {
        PaneDisplayMode = NavigationViewPaneDisplayMode.Left,
        IsBackButtonVisible = NavigationViewBackButtonVisible.Collapsed,
        IsSettingsVisible = false,
        IsPaneToggleButtonVisible = false,
        OpenPaneLength = 240,
    };
    private readonly ScrollViewer scroller = new() { HorizontalScrollMode = ScrollMode.Disabled };
    private readonly StackPanel page = new()
    {
        Spacing = 4,
        MaxWidth = 1000,
        Padding = new Thickness(36, 0, 36, 36),
    };
    private readonly Grid notebookHost = new();
    private readonly InfoBar banner = new() { IsClosable = true, Margin = new Thickness(36, 0, 36, 8) };
    private readonly TextBlock generalFooter = new()
    {
        Text = "Copyright © 2026 FueTsui. All Rights Reserved.",
        Style = Res<Style>("SecondaryTextStyle"),
        HorizontalAlignment = HorizontalAlignment.Center,
        TextAlignment = TextAlignment.Center,
        Margin = new Thickness(36, 8, 36, 16),
        Visibility = Visibility.Collapsed,
    };
    private readonly PointerEventHandler rootPointerPressed;

    private string current = "dictation";
    private bool applying;
    private bool settingsClosed = true;
    private bool closingSettings;
    private bool settingsSaveFailureShown;
    private int pageVersion;
    private Action<string, JsonObject>? pageEvents;
    private Action? pageClosing;
    private Action? resumePage;
    private (string Tag, JsonNode? Settings)? retainedPage;
    private NotebookView? notebook;
    private ToggleSwitch? dictationTextToggle;

    public SettingsWindow(App app)
    {
        this.app = app;
        s = app.Settings;
        Title = Loc.T("settings.title");

        var titleBar = new Grid { Height = 48, Padding = new Thickness(16, 0, 0, 0) };
        var titleRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12, VerticalAlignment = VerticalAlignment.Center };
        titleRow.Children.Add(new FontIcon { Glyph = "", FontSize = 16 });
        titleRow.Children.Add(new TextBlock
        {
            Text = Loc.T("settings.title"),
            Style = Res<Style>("CaptionTextBlockStyle"),
            VerticalAlignment = VerticalAlignment.Center,
        });
        titleBar.Children.Add(titleRow);

        foreach (var (tag, glyph) in Pages)
        {
            nav.MenuItems.Add(new NavigationViewItem
            {
                Content = Loc.T("nav." + tag),
                Icon = new FontIcon { Glyph = glyph },
                Tag = tag,
            });
        }
        nav.SelectionChanged += OnNavigationChanged;

        var content = new Grid();
        content.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        content.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        content.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        scroller.Content = page;
        Grid.SetRow(scroller, 1);
        content.Children.Add(banner);
        content.Children.Add(scroller);
        notebookHost.Visibility = Visibility.Collapsed;
        Grid.SetRow(notebookHost, 1);
        content.Children.Add(notebookHost);
        Grid.SetRow(generalFooter, 2);
        content.Children.Add(generalFooter);
        nav.Content = content;

        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        Grid.SetRow(nav, 1);
        root.Children.Add(titleBar);
        root.Children.Add(nav);
        Content = root;

        ExtendsContentIntoTitleBar = true;
        SetTitleBar(titleBar);
        AppWindow.TitleBar.PreferredHeightOption = TitleBarHeightOption.Tall;
        SystemBackdrop = new MicaBackdrop();
        var icon = Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico");
        if (File.Exists(icon))
        {
            AppWindow.SetIcon(icon);
        }
        PlaceCentered(1060, 760);
        AppWindow.Closing += OnWindowClosing;
        Activated += OnWindowActivated;
        rootPointerPressed = (_, e) =>
        {
            if (recordingRow is not { } recording) return;
            for (DependencyObject? node = e.OriginalSource as DependencyObject; node is not null; node = VisualTreeHelper.GetParent(node))
                if (ReferenceEquals(node, recording.Input.View)) return;
            // Non-focusable labels and page whitespace do not raise LostFocus.
            // Cancel without handling the pointer so another row can still start.
            _ = StopRecordingAsync();
        };
        root.AddHandler(UIElement.PointerPressedEvent, rootPointerPressed, true);
    }

    /// <summary>False while the reused window is hidden; messages then belong on the bar.</summary>
    public bool IsOpen => !settingsClosed;

    /// <summary>Raised after the window was hidden; the app frees it when it stays unused.</summary>
    public event Action? Hidden;

    private void OnNavigationChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs e)
    {
        if (e.SelectedItem is NavigationViewItem { Tag: string tag }) ShowPage(tag);
    }

    private async void OnWindowClosing(AppWindow sender, AppWindowClosingEventArgs e)
    {
        e.Cancel = true;
        if (closingSettings) return;
        closingSettings = true;
        try { await CloseAfterSavingAsync(); }
        finally { closingSettings = false; }
    }

    private void OnWindowActivated(object sender, WindowActivatedEventArgs e)
    {
        if (!settingsClosed && e.WindowActivationState == WindowActivationState.Deactivated) _ = StopRecordingAsync();
    }

    /// <summary>Checks drafts before either the window or the entire app can close.</summary>
    public async Task<bool> CloseAfterSavingAsync()
    {
        if (settingsClosed) return true;
        await StopRecordingAsync();
        // Shortcut fields update the local settings tree optimistically. Do not
        // close away their failure/retry state while the write is still unsaved.
        if (!await s.SaveNowAsync())
        {
            ShowSettingsSaveFailure("save_failed");
            return false;
        }
        if (notebook is not null && !await notebook.FlushAsync())
        {
            ShowIssue(Loc.Error("save_failed"));
            return false;
        }
        if (!settingsClosed)
        {
            settingsClosed = true;
            pageVersion++;
            notebook?.Release();
            AppWindow.Hide();
            serviceDialog?.Hide();
            if (current is "dictation" or "general")
            {
                retainedPage = (current, PageSettings(current)?.DeepClone());
            }
            else
            {
                ReleasePage();
                page.Children.Clear();
            }
            ReleaseCardStyleTimer();
            notebookHost.Children.Clear();
            notebook = null;
            banner.IsOpen = false;
            settingsSaveFailureShown = false;
            Hidden?.Invoke();
        }
        return true;
    }

    /// <summary>Destroys the shared native host after all pending writes have finished.</summary>
    public void CloseHost()
    {
        AppWindow.Closing -= OnWindowClosing;
        Activated -= OnWindowActivated;
        nav.SelectionChanged -= OnNavigationChanged;
        root.RemoveHandler(UIElement.PointerPressedEvent, rootPointerPressed);
        ReleasePage();
        SystemBackdrop = null;
        SetTitleBar(null);
        Content = null;
        Close();
    }

    /// <summary>Shows the appearance page with one card's own style selected.</summary>
    public void OpenCardStyle(ulong id)
    {
        FlushCardStyle();
        styleSurface = "cards";
        styleCard = id;
        if (current == "appearance")
        {
            ShowPage("appearance");
        }
        Open("appearance");
    }

    /// <summary>
    /// Shows a page and brings the window to the front, restoring it when minimized. The bar
    /// never takes focus, so <see cref="Window.Activate"/> alone would leave the window behind
    /// the app the user is working in.
    /// </summary>
    public void Open(string? tag)
    {
        var reopened = settingsClosed;
        if (reopened)
        {
            settingsClosed = false;
            ApplyTheme();
            tag ??= "dictation";
        }
        if (tag is not null)
        {
            var item = nav.MenuItems.OfType<NavigationViewItem>().FirstOrDefault(i => (string)i.Tag == tag);
            if (item is not null)
            {
                if (!ReferenceEquals(nav.SelectedItem, item)) nav.SelectedItem = item;
                else if (reopened)
                {
                    if (retainedPage is { } saved && saved.Tag == tag
                        && JsonNode.DeepEquals(saved.Settings, PageSettings(tag)))
                    {
                        retainedPage = null;
                        resumePage?.Invoke();
                        scroller.ChangeView(null, 0, null, true);
                    }
                    else ShowPage(tag);
                }
            }
        }
        if (AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Minimized } presenter)
        {
            presenter.Restore();
        }
        Controls.Overlay.BringToFront(this);
    }

    private JsonNode? PageSettings(string tag) => tag == "dictation" ? s.Get("dictation")
        : new JsonArray(s.Bool("general.startWithWindows"), s.Bool("general.checkUpdatesDaily"));

    public void ApplyTheme()
    {
        if (settingsClosed) return;
        var dark = Theme.IsDark(s.Str("appearance.theme", "system"));
        var requested = dark ? ElementTheme.Dark : ElementTheme.Light;
        if (root.RequestedTheme == requested) return;
        root.RequestedTheme = requested;
        var bar = AppWindow.TitleBar;
        var transparent = Color.FromArgb(0, 0, 0, 0);
        bar.ButtonBackgroundColor = transparent;
        bar.ButtonInactiveBackgroundColor = transparent;
        bar.ButtonForegroundColor = dark ? Color.FromArgb(255, 255, 255, 255) : Color.FromArgb(228, 0, 0, 0);
        bar.ButtonInactiveForegroundColor = dark ? Color.FromArgb(120, 255, 255, 255) : Color.FromArgb(110, 0, 0, 0);
        bar.ButtonHoverBackgroundColor = dark ? Color.FromArgb(20, 255, 255, 255) : Color.FromArgb(14, 0, 0, 0);
        bar.ButtonHoverForegroundColor = bar.ButtonForegroundColor;
        bar.ButtonPressedBackgroundColor = dark ? Color.FromArgb(12, 255, 255, 255) : Color.FromArgb(9, 0, 0, 0);
        bar.ButtonPressedForegroundColor = bar.ButtonForegroundColor;
        if (!applying) afterOwnChange?.Invoke();
    }

    public void OnEngineEvent(string name, JsonObject e)
    {
        if (settingsClosed) return;
        if (name == "cards.changed") RetireSavedCardStyle();
        pageEvents?.Invoke(name, e);
        // The notebook stays alive while another page is shown; it keeps up with changes so
        // it is current when it comes back.
        notebook?.OnEngineEvent(name, e);
    }

    /// <summary>Rebuilds the page when a setting changed somewhere else, e.g. on the bar.</summary>
    public void OnSettingsChanged(string path)
    {
        // The window theme and its preview have already been updated by ApplyTheme.
        if (settingsClosed || path == "appearance.theme") return;
        if (path is "*" or "hotkeys" or "hotkeys.note") notebook?.RefreshShortcutHint();
        if (current == "shortcuts")
        {
            // Settings acknowledgements must not replace a focused recorder or
            // destroy an unsaved shortcut candidate. Idle values update in place.
            refreshShortcutValues?.Invoke();
            return;
        }
        if (settingsClosed || serviceDialog is not null || applying || current == "notebook" || path.StartsWith("general.barPosition", StringComparison.Ordinal)
            || path.StartsWith("general.barVisualCenter", StringComparison.Ordinal)
            || path.StartsWith("general.captionsPosition", StringComparison.Ordinal)
            || path.StartsWith("general.barPinned", StringComparison.Ordinal)
            || path.StartsWith("general.closeHintShown", StringComparison.Ordinal))
        {
            return;
        }
        if (path == "dictation.showText" && dictationTextToggle is not null)
        {
            dictationTextToggle.IsOn = s.Bool(path);
            return;
        }
        if (root.XamlRoot is not null && FocusManager.GetFocusedElement(root.XamlRoot) is TextBox or RichEditBox or PasswordBox or NumberBox)
        {
            return;
        }
        ShowPage(current, keepScroll: true);
    }

    public void ShowIssue(string message)
    {
        if (settingsClosed) return;
        settingsSaveFailureShown = false;
        banner.Severity = InfoBarSeverity.Warning;
        banner.Message = message;
        banner.IsOpen = true;
    }

    public void ShowSettingsSaveFailure(string code)
    {
        if (settingsClosed) return;
        if (shortcutSettingsSaving || current == "shortcuts" && shortcutDrafts.Count > 0)
        {
            // A shortcut failure belongs to its own description. Keep that draft
            // available across page changes instead of raising a global banner.
            foreach (var draft in shortcutDrafts.Values.Where(draft => draft.Saving))
                draft.Message = Loc.T("key.unsaved");
            refreshShortcutValues?.Invoke();
            ClearSavedSettingsIssue();
            return;
        }
        ShowIssue(Loc.Error(code == "stopped" ? code : "save_failed"));
        settingsSaveFailureShown = code != "stopped";
    }

    private void ClearSavedSettingsIssue()
    {
        if (settingsClosed || !settingsSaveFailureShown) return;
        settingsSaveFailureShown = false;
        banner.IsOpen = false;
    }

    private void ShowPage(string tag, bool keepScroll = false)
    {
        if (settingsClosed) return;
        if (current == "notebook" && tag != "notebook" && notebook?.FinishEditing() == false)
        {
            nav.SelectedItem = nav.MenuItems.OfType<NavigationViewItem>().FirstOrDefault(item => item.Tag as string == current);
            return;
        }
        pageVersion++;
        var offset = scroller.VerticalOffset;
        ReleasePage();
        current = tag;
        page.Children.Clear();
        generalFooter.Visibility = tag == "general" ? Visibility.Visible : Visibility.Collapsed;
        var isNotebook = tag == "notebook";
        scroller.Visibility = isNotebook ? Visibility.Collapsed : Visibility.Visible;
        notebookHost.Visibility = isNotebook ? Visibility.Visible : Visibility.Collapsed;
        if (isNotebook)
        {
            if (notebook is null)
            {
                notebook = new NotebookView(app, this);
                notebookHost.Children.Add(notebook.View);
            }
            return;
        }
        page.Children.Add(new TextBlock
        {
            Text = Loc.T("nav." + tag),
            Style = Res<Style>("TitleTextBlockStyle"),
            Margin = new Thickness(0, 12, 0, 16),
        });
        switch (tag)
        {
            case "dictation":
                BuildDictation();
                break;
            case "captions":
                BuildCaptions();
                break;
            case "shortcuts":
                BuildShortcuts();
                break;
            case "appearance":
                BuildAppearance();
                break;
            case "services":
                // Built when the provider list arrives; the scroll position is restored then.
                _ = BuildServicesAsync(pageVersion, keepScroll ? offset : 0);
                return;
            case "general":
                BuildGeneral();
                break;
        }
        if (keepScroll)
        {
            page.UpdateLayout();
            scroller.ChangeView(null, offset, null, true);
        }
        else
        {
            scroller.ChangeView(null, 0, null, true);
        }
    }

    private void ReleasePage()
    {
        var release = pageClosing;
        pageClosing = null;
        resumePage = null;
        retainedPage = null;
        pageEvents = null;
        afterOwnChange = null;
        refreshShortcutValues = null;
        dictationTextToggle = null;
        release?.Invoke();
    }

    private void PlaceCentered(int widthDip, int heightDip)
    {
        var work = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Primary).WorkArea;
        var scale = Controls.Overlay.Scale(this);
        var width = Math.Min((int)(widthDip * scale), work.Width);
        var height = Math.Min((int)(heightDip * scale), work.Height);
        AppWindow.MoveAndResize(new RectInt32(work.X + (work.Width - width) / 2, work.Y + (work.Height - height) / 2, width, height));
    }

    // ---- building blocks ---------------------------------------------------------------

    private static T Res<T>(string key) => (T)Application.Current.Resources[key];

    private void Set(string path, JsonNode? value)
    {
        applying = true;
        try
        {
            s.Set(path, value);
        }
        finally
        {
            applying = false;
        }
        if (!settingsClosed) afterOwnChange?.Invoke();
    }

    private void Section(string title, string? desc = null)
    {
        page.Children.Add(new TextBlock { Text = title, Style = Res<Style>("SectionHeaderStyle") });
        if (desc is not null)
        {
            page.Children.Add(new TextBlock { Text = desc, Style = Res<Style>("SecondaryTextStyle"), Margin = new Thickness(1, -2, 0, 8) });
        }
    }

    private static StackPanel Texts(string title, string? desc, out TextBlock descBlock)
    {
        var texts = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        texts.Children.Add(new TextBlock { Text = title, TextWrapping = TextWrapping.Wrap });
        descBlock = new TextBlock { Text = desc ?? "", Style = Res<Style>("SecondaryTextStyle") };
        descBlock.Visibility = string.IsNullOrEmpty(desc) ? Visibility.Collapsed : Visibility.Visible;
        texts.Children.Add(descBlock);
        return texts;
    }

    /// <summary>A card with text on the left and a control on the right.</summary>
    private Border Card(string title, string? desc, FrameworkElement? control, string? glyph = null) =>
        Card(title, desc, control, glyph, out _);

    /// <param name="below">Optional element placed under the description, such as a status line.</param>
    /// <param name="footer">Optional row below the icon, title and controls.</param>
    private Border Card(string title, string? desc, FrameworkElement? control, string? glyph, out TextBlock descBlock,
        UIElement? below = null, FrameworkElement? footer = null)
    {
        var grid = new Grid { ColumnSpacing = 16 };
        var column = 0;
        if (glyph is not null)
        {
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            grid.Children.Add(new FontIcon { Glyph = glyph, FontSize = 18, VerticalAlignment = VerticalAlignment.Center });
            column = 1;
        }
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var texts = Texts(title, desc, out descBlock);
        if (below is not null)
        {
            texts.Children.Add(below);
        }
        Grid.SetColumn(texts, column);
        grid.Children.Add(texts);
        if (control is not null)
        {
            control.VerticalAlignment = VerticalAlignment.Center;
            control.HorizontalAlignment = HorizontalAlignment.Right;
            Grid.SetColumn(control, column + 1);
            grid.Children.Add(control);
        }
        if (footer is not null)
        {
            grid.RowDefinitions.Add(new RowDefinition());
            grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            Grid.SetRow(footer, 1);
            Grid.SetColumn(footer, column);
            Grid.SetColumnSpan(footer, 2);
            grid.Children.Add(footer);
        }
        var card = new Border { Style = Res<Style>("CardStyle"), Child = grid };
        page.Children.Add(card);
        return card;
    }

    /// <summary>A card with text on top and a larger control below.</summary>
    private Border Block(string title, string? desc, UIElement content)
    {
        var stack = new StackPanel { Spacing = 10 };
        stack.Children.Add(Texts(title, desc, out _));
        stack.Children.Add(content);
        var card = new Border { Style = Res<Style>("CardStyle"), Child = stack };
        page.Children.Add(card);
        return card;
    }

    private ToggleSwitch Toggle(string path, Action<bool>? changed = null)
    {
        var toggle = new ToggleSwitch { IsOn = s.Bool(path), MinWidth = 0 };
        toggle.Toggled += (_, _) =>
        {
            if (settingsClosed || toggle.IsOn == s.Bool(path)) return;
            Set(path, toggle.IsOn);
            changed?.Invoke(toggle.IsOn);
        };
        return toggle;
    }

    private RadioButtons Radios(string path, params (string Value, string Label)[] options)
    {
        var radios = new RadioButtons();
        foreach (var (_, label) in options)
        {
            radios.Items.Add(label);
        }
        radios.SelectedIndex = Math.Max(0, Array.FindIndex(options, o => o.Value == s.Str(path)));
        radios.SelectionChanged += (_, _) =>
        {
            if (radios.SelectedIndex >= 0)
            {
                Set(path, options[radios.SelectedIndex].Value);
            }
        };
        return radios;
    }

    private ComboBox Combo(string path, IEnumerable<(string Value, string Label)> options, double width = 220, Action<string>? changed = null)
    {
        var combo = new ComboBox { MinWidth = width };
        var list = options.ToList();
        var value = s.Str(path);
        if (list.All(o => o.Value != value))
        {
            list.Add((value, value));
        }
        foreach (var (v, label) in list)
        {
            combo.Items.Add(new ComboBoxItem { Content = label, Tag = v });
        }
        combo.SelectedIndex = list.FindIndex(o => o.Value == value);
        combo.SelectionChanged += (_, _) =>
        {
            if (!settingsClosed && combo.SelectedItem is ComboBoxItem { Tag: string v } && v != s.Str(path))
            {
                Set(path, v);
                changed?.Invoke(v);
            }
        };
        return combo;
    }

    private NumberBox Number(string path, double min, double max)
    {
        var box = new NumberBox
        {
            Minimum = min,
            Maximum = max,
            SmallChange = 1,
            LargeChange = 5,
            Value = s.Num(path, min),
            SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Compact,
            MinWidth = 120,
        };
        box.ValueChanged += (_, e) =>
        {
            if (!double.IsNaN(e.NewValue))
            {
                Set(path, (int)Math.Round(Math.Clamp(e.NewValue, min, max)));
            }
        };
        return box;
    }

    private Slider SliderFor(string path, double min, double max, double step, bool integer, double width = 220)
    {
        var slider = new Slider
        {
            Minimum = min,
            Maximum = max,
            StepFrequency = step,
            Value = s.Num(path, min),
            Width = width,
        };
        slider.ValueChanged += (_, e) =>
        {
            if (integer)
            {
                Set(path, (int)Math.Round(e.NewValue));
            }
            else
            {
                Set(path, Math.Round(e.NewValue, 1));
            }
        };
        return slider;
    }

    private TextBox Text(string path, string header, string placeholder, double width = 420)
    {
        var box = new TextBox
        {
            Header = header,
            PlaceholderText = placeholder,
            Text = s.Str(path),
            Width = width,
            HorizontalAlignment = HorizontalAlignment.Left,
        };
        box.TextChanged += (_, _) => Set(path, box.Text.Trim());
        return box;
    }

    private static TextBlock Note(string text = "") => new()
    {
        Text = text,
        Style = Res<Style>("SecondaryTextStyle"),
        Margin = new Thickness(0, 4, 0, 0),
    };

    private static Button ActionButton(string text, Action click, bool accent = false)
    {
        var button = new Button { Content = text, MinWidth = 88 };
        if (accent)
        {
            button.Style = Res<Style>("AccentButtonStyle");
        }
        button.Click += (_, _) => click();
        return button;
    }

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
