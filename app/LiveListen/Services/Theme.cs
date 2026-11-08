using System.Globalization;
using System.Text.Json.Nodes;
using Microsoft.UI.Xaml;
using Windows.UI;
using Windows.UI.ViewManagement;

namespace LiveListen.Services;

/// <summary>Theme, color and shortcut-name helpers shared by all windows.</summary>
internal static class Theme
{
    private static readonly UISettings Ui = new();

    public static bool AnimationsEnabled => Ui.AnimationsEnabled;

    public static bool SystemDark => Ui.GetColorValue(UIColorType.Background).R < 128;

    public static bool IsDark(string setting) => setting == "dark" || (setting != "light" && SystemDark);

    public static ElementTheme Element(string setting) => IsDark(setting) ? ElementTheme.Dark : ElementTheme.Light;

    /// <summary>Raised on a background thread when Windows colors or theme change.</summary>
    public static event Action? SystemChanged;

    static Theme()
    {
        Ui.ColorValuesChanged += (_, _) => SystemChanged?.Invoke();
        Ui.AnimationsEnabledChanged += (_, _) => SystemChanged?.Invoke();
    }

    public static Color Accent(bool dark) => Ui.GetColorValue(dark ? UIColorType.AccentLight2 : UIColorType.AccentDark1);

    /// <summary>Parses <c>#AARRGGBB</c> or <c>#RRGGBB</c>.</summary>
    public static Color? Parse(string? text)
    {
        if (string.IsNullOrWhiteSpace(text) || text[0] != '#')
        {
            return null;
        }
        var hex = text[1..];
        if (!uint.TryParse(hex, NumberStyles.HexNumber, CultureInfo.InvariantCulture, out var v))
        {
            return null;
        }
        return hex.Length switch
        {
            8 => Color.FromArgb((byte)(v >> 24), (byte)(v >> 16), (byte)(v >> 8), (byte)v),
            6 => Color.FromArgb(255, (byte)(v >> 16), (byte)(v >> 8), (byte)v),
            _ => null,
        };
    }

    public static string Format(Color c) => $"#{c.A:X2}{c.R:X2}{c.G:X2}{c.B:X2}";

    public static Color WithAlpha(Color c, byte a) => Color.FromArgb(a, c.R, c.G, c.B);

    /// <summary>Default foreground for text on the transparent desktop overlay.</summary>
    public static Color VoiceText(SettingsStore settings) =>
        VoiceTextColor(Parse(settings.Str("appearance.bar.text")) ??
            (IsDark(settings.Str("appearance.theme", "system")) ? Color.FromArgb(255, 245, 245, 245) : Color.FromArgb(255, 24, 24, 24)));

    /// <summary>Softens pure black/white dictation text, preserving alpha and custom hues.</summary>
    private static Color VoiceTextColor(Color color) => (color.R, color.G, color.B) switch
    {
        (0, 0, 0) => Color.FromArgb(color.A, 24, 24, 24),
        (255, 255, 255) => Color.FromArgb(color.A, 245, 245, 245),
        _ => color,
    };

    /// <summary>Theme default colors for a surface when the user left a color empty.</summary>
    public static (Color Text, Color Background, Color Border) Defaults(bool dark) => dark
        ? (Color.FromArgb(255, 255, 255, 255), Color.FromArgb(255, 44, 44, 44), Color.FromArgb(40, 255, 255, 255))
        : (Color.FromArgb(230, 0, 0, 0), Color.FromArgb(255, 249, 249, 249), Color.FromArgb(26, 0, 0, 0));

    /// <summary>
    /// Resolved style of one surface (bar, captions, cards). <paramref name="own"/> is a card's
    /// own style; without it the surface's style in the settings is used.
    /// </summary>
    public static SurfaceLook Look(SettingsStore s, string surface, JsonObject? own = null)
    {
        var theme = s.Str("appearance.theme", "system");
        if (own is not null)
        {
            return Look(theme, key => own[key]);
        }
        var p = "appearance." + surface + ".";
        var look = Look(theme, key => s.Get(p + key));
        // Empty legacy/default colors resolve transparently for dictation only.
        // Explicit saved colors, subtitle defaults and card styles are preserved.
        return surface == "bar" ? look with
        {
            Text = VoiceText(s),
            Background = Parse(s.Str(p + "background")) ?? Color.FromArgb(0, 0, 0, 0),
            Border = Parse(s.Str(p + "border")) ?? Color.FromArgb(0, 0, 0, 0),
            Shadow = string.IsNullOrWhiteSpace(s.Str(p + "shadow")) ? "none" : look.Shadow,
        } : look;
    }

    /// <summary>Resolves a style whose fields are read with <paramref name="field"/>.</summary>
    public static SurfaceLook Look(string theme, Func<string, JsonNode?> field)
    {
        var dark = IsDark(theme);
        var custom = Parse(field("background").Str());
        if (custom is { A: >= 80 } c)
        {
            // Controls on the surface follow how light the chosen background is.
            dark = 0.299 * c.R + 0.587 * c.G + 0.114 * c.B < 128;
        }
        var (text, background, border) = Defaults(dark);
        return new SurfaceLook(
            dark,
            Parse(field("text").Str()) ?? text,
            custom ?? background,
            Parse(field("border").Str()) ?? border,
            (float)field("cornerRadius").Num(8),
            field("shadow").Str("soft"),
            field("padding").Num(12));
    }

    /// <summary>Friendlier shortcut text, e.g. "RCtrl" becomes "右 Ctrl".</summary>
    public static string Keys(string binding)
    {
        if (string.IsNullOrWhiteSpace(binding))
        {
            return Loc.T("key.none");
        }
        var right = Loc.English ? "Right " : "右 ";
        var left = Loc.English ? "Left " : "左 ";
        return string.Join(" + ", binding.Split('+').Select(part => part switch
        {
            "RCtrl" => right + "Ctrl",
            "LCtrl" => left + "Ctrl",
            "RAlt" => right + "Alt",
            "LAlt" => left + "Alt",
            "RShift" => right + "Shift",
            "LShift" => left + "Shift",
            "RWin" => right + "Win",
            "LWin" => left + "Win",
            _ => part,
        }));
    }
}

internal readonly record struct SurfaceLook(
    bool Dark,
    Color Text,
    Color Background,
    Color Border,
    float Radius,
    string Shadow,
    double Padding)
{
    /// <summary>Room around the surface for its shadow, in DIPs.</summary>
    public double ShadowMargin => Shadow switch
    {
        "strong" => 24,
        "soft" => 16,
        _ => 2,
    };

    public Color Secondary => Theme.WithAlpha(Text, (byte)(Text.A * 0.62));

    public Color Hover => Dark ? Color.FromArgb(20, 255, 255, 255) : Color.FromArgb(14, 0, 0, 0);

    public Color Pressed => Dark ? Color.FromArgb(12, 255, 255, 255) : Color.FromArgb(9, 0, 0, 0);
}
