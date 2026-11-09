using System.Globalization;
using System.Text;

namespace LiveListen.Services;

/// <summary>Card header formatting only; never changes a note or its creation timestamp.</summary>
internal static class CardHeaderInfo
{
    public const string DefaultMode = "title";
    public const int MaxTitleLength = 80;
    public static readonly string[] Modes =
        ["none", "yearMonthDay", "monthDay", "monthDayTime", "monthDayWeekday", "monthDayTitle", "weekdayTitle", "title"];

    public static string Mode(string? value) => Modes.Contains(value) ? value! : DefaultMode;
    public static bool UsesDate(string? mode) => Mode(mode) is not ("none" or "title");
    public static bool UsesTitle(string? mode) => Mode(mode) is "monthDayTitle" or "weekdayTitle" or "title";
    public static DateOnly? ParseDate(string? value) =>
        DateOnly.TryParseExact(value, "yyyy-MM-dd", CultureInfo.InvariantCulture, DateTimeStyles.None, out var date) ? date : null;

    public static string CleanTitle(string? value) => string.Concat((value ?? "").EnumerateRunes()
        .Select(rune => rune.Value is '\r' or '\n' or '\t' ? new Rune(' ') : rune)
        .Where(rune => !Rune.IsControl(rune)).Take(MaxTitleLength).Select(rune => rune.ToString()));

    public static string Render(string? created, string? mode, string? displayDate, string? title,
        string fallbackTitle, bool english, TimeZoneInfo? zone = null)
    {
        mode = Mode(mode);
        if (mode == "none") return "";
        var name = CleanTitle(title).Trim();
        if (name.Length == 0) name = fallbackTitle;
        if (mode == "title") return name;
        DateTimeOffset? at = DateTimeOffset.TryParse(created, CultureInfo.InvariantCulture,
            DateTimeStyles.AssumeUniversal, out var parsed) ? TimeZoneInfo.ConvertTime(parsed, zone ?? TimeZoneInfo.Local) : null;
        var date = ParseDate(displayDate) ?? (at.HasValue ? DateOnly.FromDateTime(at.Value.DateTime) : null);
        var culture = CultureInfo.GetCultureInfo(english ? "en-US" : "zh-CN");
        var monthDay = date?.ToString(english ? "MMM d" : "M月d日", culture) ?? "";
        var weekday = date?.ToString("dddd", culture) ?? "";
        static string Join(params string[] parts) => string.Join(" - ", parts.Where(part => part.Length > 0));
        return mode switch
        {
            "monthDay" => monthDay,
            "monthDayTime" => Join(monthDay, at?.ToString("HH:mm", CultureInfo.InvariantCulture) ?? ""),
            "monthDayWeekday" => Join(monthDay, weekday),
            "monthDayTitle" => Join(monthDay, name),
            "weekdayTitle" => Join(weekday, name),
            _ => date?.ToString(english ? "MMM d yyyy" : "yyyy年M月d日", culture) ?? "",
        };
    }
}
