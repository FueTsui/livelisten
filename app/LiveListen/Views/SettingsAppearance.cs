using System.Text.Json.Nodes;
using LiveListen.Controls;
using LiveListen.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace LiveListen.Views;

/// <summary>The appearance page: theme, language and the style of each surface.</summary>
internal sealed partial class SettingsWindow
{
    private string styleSurface
    {
        get => s.Str("appearance.styleSurface", "bar");
        set { if (value != styleSurface) Set("appearance.styleSurface", value); }
    }

    /// <summary>Where the style editor reads and writes: the settings, or one card.</summary>
    private sealed record StyleTarget(Func<string, JsonNode?> Get, Action<string, JsonNode?> Set, Func<SurfaceLook> Look);

    private void BuildAppearance()
    {
        Card(T("look.theme.title"), null, Combo("appearance.theme",
            [("system", T("theme.system")), ("light", T("theme.light")), ("dark", T("theme.dark"))]), "");
        Card(T("look.language.title"), T("look.language.desc"), Combo("appearance.language",
            [("system", T("language.system")), ("zh-Hans", T("language.zh")), ("en", T("language.en"))]), "");
        Card(T("voiceStyle.title"), T("voiceStyle.desc"), Combo("appearance.voiceStyle",
            [("orb", T("voiceStyle.orb")), ("ribbon", T("voiceStyle.ribbon"))]), "");

        Section(T("look.surface.title"));
        string[] surfaces = ["bar", "captions", "cards"];
        var picker = new RadioButtons { MaxColumns = 3 };
        foreach (var surface in surfaces)
        {
            picker.Items.Add(T("surface." + surface));
        }
        picker.SelectedIndex = Array.IndexOf(surfaces, styleSurface);
        picker.SelectionChanged += (_, _) =>
        {
            if (picker.SelectedIndex >= 0 && surfaces[picker.SelectedIndex] != styleSurface)
            {
                FlushCardStyle();
                styleSurface = surfaces[picker.SelectedIndex];
                ShowPage(current, keepScroll: true);
            }
        };
        page.Children.Add(picker);
        if (styleSurface == "cards")
        {
            BuildCardStyle();
        }
        else
        {
            BuildStyleEditor(styleSurface, SettingsTarget(styleSurface), T("look.reset"), () => ResetSurface(styleSurface));
        }
    }

    private StyleTarget SettingsTarget(string surface)
    {
        var p = "appearance." + surface + ".";
        return new StyleTarget(key => s.Get(p + key), (key, value) => Set(p + key, value),
            () => Theme.Look(s, surface));
    }

    private async void ResetSurface(string surface)
    {
        var version = pageVersion;
        var all = await s.DefaultsAsync();
        if (settingsClosed || version != pageVersion) return;
        if ((all["appearance"] as JsonObject)?[surface] is JsonObject defaults)
        {
            if (surface == "bar")
            {
                Set("dictation.fontSize", 16);
                Set("appearance.voiceTextGap", 8);
            }
            Set("appearance." + surface, defaults.DeepClone());
            ShowPage(current, keepScroll: true);
        }
    }

    private void BuildStyleEditor(string surface, StyleTarget target, string resetText, Action reset, Action? afterPreview = null)
    {
        var previewHost = new Grid();
        var previewIcon = new FontIcon
        {
            Glyph = surface switch { "bar" => "", "captions" => "", _ => "" },
            FontSize = 16,
            VerticalAlignment = VerticalAlignment.Center,
        };
        var previewText = new TextBlock
        {
            Text = T("look.preview.text"),
            TextWrapping = TextWrapping.Wrap,
            VerticalAlignment = VerticalAlignment.Center,
            MaxWidth = 300,
        };
        var previewContent = Horizontal(previewIcon, previewText);
        previewContent.Spacing = 12;
        var previewFrame = new Grid
        {
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(28),
        };
        previewFrame.Children.Add(previewHost);
        previewFrame.Children.Add(previewContent);
        page.Children.Add(new Border
        {
            Style = Res<Style>("PreviewAreaStyle"),
            Child = previewFrame,
            MinHeight = 150,
            Margin = new Thickness(0, 8, 0, 4),
        });
        afterPreview?.Invoke();
        var preview = new Surface(previewHost);
        pageClosing += preview.ReleaseShadow;
        var colorLabels = new List<Action>();
        void Repaint()
        {
            var look = target.Look();
            preview.Apply(look);
            previewContent.Padding = new Thickness(Math.Max(4, look.Padding + 4), look.Padding, Math.Max(4, look.Padding + 4), look.Padding);
            var brush = new SolidColorBrush(look.Text);
            previewText.Foreground = brush;
            previewIcon.Foreground = brush;
            previewText.FontSize = surface == "captions" ? Math.Clamp(s.Num("captions.fontSize", 24), 12, 64)
                : surface == "bar" ? Math.Clamp(s.Num("dictation.fontSize", 16), 12, 64) : 14;
            foreach (var update in colorLabels)
            {
                update();
            }
        }
        afterOwnChange = Repaint;

        if (surface == "bar")
        {
            Card(T("dict.font.title"), null, DictationFontSlider());
            var gap = new Slider { Minimum = 0, Maximum = 64, StepFrequency = 1, Width = 220,
                Value = Math.Clamp(s.Num("appearance.voiceTextGap", 8), 0, 64) };
            gap.ValueChanged += (_, e) => Set("appearance.voiceTextGap", Math.Round(e.NewValue));
            Card(T("voice.textGap.title"), null, gap);
        }
        Card(T("look.text"), null, ColorButton(target, "text", 0, colorLabels));
        Card(T("look.background"), null, ColorButton(target, "background", 1, colorLabels));
        Card(T("look.border"), null, ColorButton(target, "border", 2, colorLabels));
        Card(T("look.radius"), null, StyleSlider(target, "cornerRadius", 0, 32, 8));
        Card(T("look.padding"), null, StyleSlider(target, "padding", 0, 48, 12));
        string[] shadows = ["none", "soft", "strong"];
        var shadow = new RadioButtons();
        foreach (var value in shadows)
        {
            shadow.Items.Add(T("shadow." + value));
        }
        shadow.SelectedIndex = Math.Max(0, Array.IndexOf(shadows, target.Look().Shadow));
        shadow.SelectionChanged += (_, _) =>
        {
            if (shadow.SelectedIndex >= 0 && shadows[shadow.SelectedIndex] != target.Look().Shadow)
            {
                target.Set("shadow", shadows[shadow.SelectedIndex]);
            }
        };
        Block(T("look.shadow"), null, shadow);
        var resetButton = ActionButton(resetText, reset);
        resetButton.Margin = new Thickness(0, 12, 0, 0);
        page.Children.Add(resetButton);
        Repaint();
    }

    private static Slider StyleSlider(StyleTarget target, string key, double min, double max, double fallback)
    {
        var slider = new Slider
        {
            Minimum = min,
            Maximum = max,
            StepFrequency = 1,
            Value = Math.Clamp(target.Get(key).Num(fallback), min, max),
            Width = 220,
        };
        slider.ValueChanged += (_, e) =>
        {
            var value = Math.Round(e.NewValue, 1);
            if (value != target.Get(key).Num(fallback)) target.Set(key, value);
        };
        return slider;
    }

    private static Button ColorButton(StyleTarget target, string key, int role, List<Action> labels)
    {
        var swatch = new Border
        {
            Width = 20,
            Height = 20,
            CornerRadius = new CornerRadius(4),
            BorderThickness = new Thickness(1),
            BorderBrush = new SolidColorBrush(Color.FromArgb(80, 128, 128, 128)),
        };
        var label = new TextBlock { VerticalAlignment = VerticalAlignment.Center, MinWidth = 96 };
        var button = new Button { Content = Horizontal(swatch, label) };
        Color Current()
        {
            var look = target.Look();
            return role switch { 0 => look.Text, 1 => look.Background, _ => look.Border };
        }
        void Show()
        {
            swatch.Background = new SolidColorBrush(Current());
            var raw = target.Get(key).Str();
            label.Text = raw.Length == 0 ? T("color.auto") : raw;
        }
        Show();
        labels.Add(Show);

        var picker = new ColorPicker
        {
            IsAlphaEnabled = true,
            IsMoreButtonVisible = false,
            IsColorChannelTextInputVisible = false,
            IsHexInputVisible = true,
        };
        var auto = new Button { Content = T("color.auto"), HorizontalAlignment = HorizontalAlignment.Stretch };
        var content = new StackPanel { Spacing = 12 };
        content.Children.Add(picker);
        content.Children.Add(auto);
        var flyout = new Flyout { Content = content, Placement = FlyoutPlacementMode.Bottom };
        var loading = false;
        flyout.Opening += (_, _) =>
        {
            loading = true;
            picker.Color = Current();
            loading = false;
        };
        picker.ColorChanged += (_, e) =>
        {
            if (!loading)
            {
                target.Set(key, Theme.Format(e.NewColor));
            }
        };
        auto.Click += (_, _) =>
        {
            target.Set(key, "");
            flyout.Hide();
        };
        button.Flyout = flyout;
        return button;
    }
}
