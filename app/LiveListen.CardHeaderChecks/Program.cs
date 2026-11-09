using LiveListen.Services;

// Pure formatting checks: no GUI, native DLL, hooks, credentials or user data.
var zone = TimeZoneInfo.CreateCustomTimeZone("UTC+08", TimeSpan.FromHours(8), "UTC+08", "UTC+08");
const string created = "2026-11-08T01:05:00Z";
var passed = 0;
void Check(string name, bool condition)
{
    if (!condition) throw new InvalidOperationException(name);
    Console.WriteLine("PASS " + name);
    passed++;
}
string Render(string mode, string? date = null, string? title = "自定义", bool english = false, string stamp = created) =>
    CardHeaderInfo.Render(stamp, mode, date, title, english ? "To-do" : "待办", english, zone);

foreach (var (mode, expected) in new (string, string)[]
{
    ("none", ""), ("yearMonthDay", "2026年11月8日"), ("monthDay", "11月8日"),
    ("monthDayTime", "11月8日 - 09:05"), ("monthDayWeekday", "11月8日 - 星期日"),
    ("monthDayTitle", "11月8日 - 自定义"), ("weekdayTitle", "星期日 - 自定义"), ("title", "自定义"),
}) Check("mode " + mode, Render(mode) == expected);

Check("display date keeps original local time", Render("monthDayTime", "2028-02-29") == "2月29日 - 09:05");
Check("weekday follows the selected display date", Render("weekdayTitle", "2028-02-29") == "星期二 - 自定义");
Check("UTC creation uses local calendar day", Render("yearMonthDay", stamp: "2026-11-07T17:00:00Z") == "2026年11月8日");
Check("blank custom title uses card kind", Render("title", title: " \t ") == "待办");
Check("unknown mode defaults to title", Render("old-mode", "2026-02-29") == "自定义");
Check("invalid timestamp does not invent a date", Render("monthDayTitle", stamp: "invalid") == "自定义");
Check("chosen date can be shown without an invalid creation time", Render("monthDayTime", "2028-02-29", stamp: "invalid") == "2月29日");
Check("English headers use English calendar text", Render("monthDayWeekday", english: true) == "Nov 8 - Sunday");
Check("date parsing requires an actual ISO calendar date", CardHeaderInfo.ParseDate("2000-02-29") is not null
    && CardHeaderInfo.ParseDate("1900-02-29") is null && CardHeaderInfo.ParseDate("2026-4-01") is null);
var bounded = CardHeaderInfo.CleanTitle("A\nB\t" + string.Concat(Enumerable.Repeat("🦀", 90)) + "\0");
Check("title limit preserves Unicode scalars and removes control characters", bounded.EnumerateRunes().Count() == 80
    && bounded.StartsWith("A B ") && !bounded.Contains('\0') && !bounded.Contains('\n'));
Check("conditional fields match header information", !CardHeaderInfo.UsesDate("none") && !CardHeaderInfo.UsesTitle("none")
    && !CardHeaderInfo.UsesDate("title") && CardHeaderInfo.UsesTitle("title")
    && CardHeaderInfo.UsesDate("weekdayTitle") && CardHeaderInfo.UsesTitle("weekdayTitle")
    && CardHeaderInfo.UsesDate("monthDayTime") && !CardHeaderInfo.UsesTitle("monthDayTime"));
Console.WriteLine($"{passed} checks passed");
