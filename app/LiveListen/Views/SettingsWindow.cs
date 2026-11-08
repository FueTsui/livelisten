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

    private string current = "dictation";
    private bool applying;
    private bool settingsClosed;
    private int pageVersion;
    private Action<string, JsonObject>? pageEvents;
    private Action? pageClosing;
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
        nav.SelectionChanged += (_, e) =>
        {
            if (e.SelectedItem is NavigationViewItem { Tag: string tag })
            {
                ShowPage(tag);
            }
        };

        var content = new Grid();
        content.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        content.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        scroller.Content = page;
        Grid.SetRow(scroller, 1);
        content.Children.Add(banner);
        content.Children.Add(scroller);
        notebookHost.Visibility = Visibility.Collapsed;
        Grid.SetRow(notebookHost, 1);
        content.Children.Add(notebookHost);
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
        ApplyTheme();
        PlaceCentered(1060, 760);
        Closed += (_, _) =>
        {
            settingsClosed = true;
            pageEvents = null;
            pageClosing?.Invoke();
        };
        nav.SelectedItem = nav.MenuItems[0];
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
        if (tag is not null)
        {
            var item = nav.MenuItems.OfType<NavigationViewItem>().FirstOrDefault(i => (string)i.Tag == tag);
            if (item is not null)
            {
                nav.SelectedItem = item;
            }
        }
        if (AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Minimized } presenter)
        {
            presenter.Restore();
        }
        Controls.Overlay.BringToFront(this);
    }

    public void ApplyTheme()
    {
        var dark = Theme.IsDark(s.Str("appearance.theme", "system"));
        root.RequestedTheme = dark ? ElementTheme.Dark : ElementTheme.Light;
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
        if (settingsClosed || serviceDialog is not null || applying || current == "notebook" || path.StartsWith("general.barPosition", StringComparison.Ordinal)
            || path.StartsWith("general.barVisualCenter", StringComparison.Ordinal)
            || path.StartsWith("general.captionsPosition", StringComparison.Ordinal)
            || path.StartsWith("general.closeHintShown", StringComparison.Ordinal))
        {
            return;
        }
        if (path == "dictation.showText" && dictationTextToggle is not null)
        {
            dictationTextToggle.IsOn = s.Bool(path);
            return;
        }
        if (root.XamlRoot is not null && FocusManager.GetFocusedElement(root.XamlRoot) is TextBox or PasswordBox or NumberBox)
        {
            return;
        }
        ShowPage(current, keepScroll: true);
    }

    public void ShowIssue(string message)
    {
        if (settingsClosed) return;
        banner.Severity = InfoBarSeverity.Warning;
        banner.Message = message;
        banner.IsOpen = true;
    }

    private void ShowPage(string tag, bool keepScroll = false)
    {
        if (settingsClosed) return;
        pageVersion++;
        var offset = scroller.VerticalOffset;
        pageClosing?.Invoke();
        pageClosing = null;
        pageEvents = null;
        afterOwnChange = null;
        dictationTextToggle = null;
        current = tag;
        page.Children.Clear();
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
    private Border Card(string title, string? desc, FrameworkElement? control, string? glyph, out TextBlock descBlock, UIElement? below = null)
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
