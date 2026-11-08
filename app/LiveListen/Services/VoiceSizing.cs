namespace LiveListen.Services;

internal static class VoiceSizing
{
    public const double Minimum = 1;
    public const double Maximum = 3;
    public const double RibbonMinimum = 1;
    public const double RibbonMaximum = 3;
    public const double RibbonDefault = 1.5;
    public const double Speed = 0.5;

    public static double Clamp(double value, bool ribbon = false) => double.IsFinite(value)
        ? Math.Clamp(value, ribbon ? RibbonMinimum : Minimum, ribbon ? RibbonMaximum : Maximum)
        : ribbon ? RibbonDefault : 1.5;

    // Every high-resolution delta contributes immediately, including partial notches.
    public static double Wheel(double current, int delta, bool ribbon = false) =>
        Clamp(Math.Round(Clamp(current, ribbon) + delta / 120.0 * 0.05, 6), ribbon);

    public static double Advance(double current, double target, double seconds) =>
        current + Math.Sign(target - current) * Math.Min(Math.Abs(target - current), Speed * Math.Max(0, seconds));

    // The lower lane wins whenever the live text still fits, including at the edge.
    public static bool LiveCaptionAbove(double roomBelow, double reach) => roomBelow < reach;
}
