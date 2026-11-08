using System.Globalization;

namespace LiveListen.Services;

/// <summary>Dates and time positions as the user reads them.</summary>
internal static class Format
{
    /// <summary>Local date of a UTC ISO 8601 stamp, without its time.</summary>
    public static string Date(string iso)
    {
        if (!DateTimeOffset.TryParse(iso, CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out var at))
        {
            return "";
        }
        var local = at.ToLocalTime();
        var sameYear = local.Year == DateTimeOffset.Now.Year;
        if (Loc.English)
        {
            return local.ToString(sameYear ? "MMM d" : "MMM d yyyy", CultureInfo.GetCultureInfo("en-US"));
        }
        return local.ToString(sameYear ? "M月d日" : "yyyy年M月d日", CultureInfo.InvariantCulture);
    }

    /// <summary>Local date and time of a UTC ISO 8601 stamp, e.g. "11月6日 14:30".</summary>
    public static string When(string iso)
    {
        if (!DateTimeOffset.TryParse(iso, CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out var at))
        {
            return "";
        }
        var local = at.ToLocalTime();
        var sameYear = local.Year == DateTimeOffset.Now.Year;
        if (Loc.English)
        {
            return local.ToString(sameYear ? "MMM d, HH:mm" : "MMM d yyyy, HH:mm", CultureInfo.GetCultureInfo("en-US"));
        }
        return local.ToString(sameYear ? "M月d日 HH:mm" : "yyyy年M月d日 HH:mm", CultureInfo.InvariantCulture);
    }

    /// <summary>A time position such as "02:15" or "1:02:15".</summary>
    public static string Clock(double ms)
    {
        var total = (long)Math.Max(0, ms) / 1000;
        var (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
        return h > 0 ? $"{h}:{m:00}:{s:00}" : $"{m:00}:{s:00}";
    }
}
