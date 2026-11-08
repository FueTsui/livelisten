namespace LiveListen.Services;

/// <summary>Only invalidate surfaces that read the changed settings section.</summary>
internal readonly record struct SettingsRefresh(bool Bar, bool Captions, bool Cards, bool Theme)
{
    public static SettingsRefresh For(string path)
    {
        var all = path is "*" or "appearance" or "appearance.theme";
        return new(
            all || Section(path, "appearance.bar") || path is "appearance.voiceStyle"
                or "appearance.voiceScale" or "appearance.ribbonScale" or "appearance.voiceTextGap"
                or "dictation" or "dictation.fontSize" or "dictation.showText",
            all || Section(path, "appearance.captions") || Section(path, "captions"),
            all || Section(path, "appearance.cards"),
            all);
    }

    private static bool Section(string path, string section) =>
        path == section || path.StartsWith(section + ".", StringComparison.Ordinal);
}
