using LiveListen.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;

namespace LiveListen.Views;

/// <summary>
/// A flyout for changing a card's date and time (local time). It has no nested pop-ups, so it
/// also works from a small desktop card window.
/// </summary>
internal static class WhenPicker
{
    /// <param name="current">The card's time in Unix milliseconds, read when the flyout opens.</param>
    /// <param name="save">Gets the chosen time in Unix milliseconds.</param>
    public static Flyout Create(Func<long> current, Func<ElementTheme> theme, Action<long> save)
    {
        var flyout = new Flyout { ShouldConstrainToRootBounds = false, Placement = FlyoutPlacementMode.BottomEdgeAlignedLeft };
        flyout.Opening += (_, _) => flyout.Content = Build(flyout, current(), theme(), save);
        return flyout;
    }

    private static StackPanel Build(Flyout flyout, long ms, ElementTheme theme, Action<long> save)
    {
        var when = ms > 0 ? DateTimeOffset.FromUnixTimeMilliseconds(ms).ToLocalTime() : DateTimeOffset.Now;
        var panel = new StackPanel { Spacing = 10, RequestedTheme = theme };
        panel.Children.Add(new TextBlock { Text = Loc.T("when.title"), Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"] });

        var calendar = new CalendarView { SelectionMode = CalendarViewSelectionMode.Single, Language = Loc.Tag };
        calendar.MinDate = new DateTimeOffset(2000, 1, 1, 0, 0, 0, TimeSpan.Zero);
        calendar.MaxDate = new DateTimeOffset(2099, 12, 31, 0, 0, 0, TimeSpan.Zero);
        calendar.SelectedDates.Add(when);
        calendar.SetDisplayDate(when);
        AutomationProperties.SetName(calendar, Loc.T("when.date"));
        panel.Children.Add(calendar);

        NumberBox Number(int value, int max, string name) => new()
        {
            Value = value,
            Minimum = 0,
            Maximum = max,
            SmallChange = 1,
            SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Inline,
            ValidationMode = NumberBoxValidationMode.InvalidInputOverwritten,
            Width = 120,
            Header = name,
        };
        var hour = Number(when.Hour, 23, Loc.T("when.hour"));
        var minute = Number(when.Minute, 59, Loc.T("when.minute"));
        panel.Children.Add(new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Children = { hour, minute } });

        var ok = new Button { Content = Loc.T("common.save"), Style = (Style)Application.Current.Resources["AccentButtonStyle"], MinWidth = 88 };
        var cancel = new Button { Content = Loc.T("common.cancel"), MinWidth = 88 };
        ok.Click += (_, _) =>
        {
            var day = calendar.SelectedDates.Count > 0 ? calendar.SelectedDates[0] : when;
            var h = double.IsNaN(hour.Value) ? when.Hour : (int)hour.Value;
            var m = double.IsNaN(minute.Value) ? when.Minute : (int)minute.Value;
            var local = new DateTime(day.Year, day.Month, day.Day, h, m, 0, DateTimeKind.Local);
            flyout.Hide();
            save(new DateTimeOffset(local).ToUnixTimeMilliseconds());
        };
        cancel.Click += (_, _) => flyout.Hide();
        panel.Children.Add(new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            HorizontalAlignment = HorizontalAlignment.Right,
            Children = { ok, cancel },
        });
        return panel;
    }
}
