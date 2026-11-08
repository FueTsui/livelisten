using LiveListen.Services;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.System;
using Windows.UI;
using Windows.UI.Core;

namespace LiveListen.Controls;

/// <summary>A focused recorder button with an integrated clear action; never a text editor.</summary>
internal sealed class HotkeyInput
{
    private readonly HotkeyCaptureState capture = new();
    private readonly TextBlock label = new() { VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis };
    private readonly Button record;
    private readonly Button clear;
    private readonly string title;
    private string binding = "";
    private bool preparing;
    public bool IsBusy { get; private set; }
    public bool IsCapturing => preparing || capture.Active;
    public bool HasInputFocus => record.FocusState != FocusState.Unfocused;
    public Border View { get; }
    public event Action? Requested;
    public event Action? Cancelled;
    public event Action<string>? Committed;
    public event Action? ClearRequested;
    public event Action? Unsupported;

    public HotkeyInput(string title, string value)
    {
        this.title = title;
        var content = new Grid { ColumnSpacing = 10 };
        content.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        content.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        content.Children.Add(new FontIcon { Glyph = "\uE765", FontSize = 18, Opacity = 0.7 });
        Grid.SetColumn(label, 1);
        content.Children.Add(label);
        record = new Button
        {
            Content = content, HorizontalContentAlignment = HorizontalAlignment.Stretch,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            Background = new SolidColorBrush(Color.FromArgb(0, 0, 0, 0)), BorderThickness = new Thickness(0),
            Padding = new Thickness(12, 8, 8, 8), MinHeight = 38, CornerRadius = new CornerRadius(4),
        };
        clear = new Button
        {
            Content = new FontIcon { Glyph = "\uE711", FontSize = 10 }, Width = 28, Height = 28,
            MinWidth = 0, MinHeight = 0, Padding = new Thickness(0), BorderThickness = new Thickness(0),
            Background = new SolidColorBrush(Color.FromArgb(0, 0, 0, 0)), Margin = new Thickness(0, 0, 5, 0),
            VerticalAlignment = VerticalAlignment.Center, AllowFocusOnInteraction = false,
        };
        var row = new Grid();
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.Children.Add(record);
        Grid.SetColumn(clear, 1);
        row.Children.Add(clear);
        View = new Border
        {
            Width = 300, BorderThickness = new Thickness(1), CornerRadius = new CornerRadius(5),
            Background = new SolidColorBrush(Color.FromArgb(255, 255, 255, 255)), Child = row,
        };
        record.Foreground = clear.Foreground = new SolidColorBrush(Color.FromArgb(255, 36, 36, 36));
        foreach (var button in new[] { record, clear })
        {
            button.Resources["ButtonBackgroundPointerOver"] = new SolidColorBrush(Color.FromArgb(255, 243, 243, 243));
            button.Resources["ButtonBackgroundPressed"] = new SolidColorBrush(Color.FromArgb(255, 235, 235, 235));
            button.Resources["ButtonForegroundPointerOver"] = record.Foreground;
            button.Resources["ButtonForegroundPressed"] = record.Foreground;
        }
        AutomationProperties.SetName(clear, Loc.F("key.clearAction", title));
        ToolTipService.SetToolTip(clear, Loc.T("key.clear"));
        ToolTipService.SetToolTip(record, Loc.T("key.inputHint"));
        record.Click += (_, _) =>
        {
            if (IsBusy) return;
            if (IsCapturing) Cancelled?.Invoke();
            else Requested?.Invoke();
        };
        clear.Click += (_, _) => { if (!IsBusy) ClearRequested?.Invoke(); };
        record.PreviewKeyDown += (_, e) => Key(e, pressed: true);
        record.PreviewKeyUp += (_, e) => Key(e, pressed: false);
        record.LostFocus += (_, _) => { if (IsCapturing) Cancelled?.Invoke(); };
        View.Unloaded += (_, _) => { if (IsCapturing) Cancelled?.Invoke(); };
        SetValue(value);
    }

    public void Prepare()
    {
        preparing = true;
        capture.Cancel();
        Render();
    }

    public void Begin()
    {
        preparing = false;
        capture.Begin(new[] { 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C }.Where(key =>
            (InputKeyboardSource.GetKeyStateForCurrentThread((VirtualKey)key) & CoreVirtualKeyStates.Down) != 0));
        Render();
    }

    public void Stop() { preparing = false; capture.Cancel(); Render(); }
    public void SetBusy(bool value) { IsBusy = value; Render(); }
    public void SetValue(string value) { binding = value; Render(); }

    private void Key(KeyRoutedEventArgs e, bool pressed)
    {
        if (!HasInputFocus || !IsCapturing && !IsBusy) return;
        e.Handled = true;
        if (pressed && e.OriginalKey == VirtualKey.Escape)
        {
            if (IsCapturing) Cancelled?.Invoke();
            return;
        }
        if (preparing || IsBusy) return;
        var physical = HotkeyCaptureState.PhysicalKey((int)e.OriginalKey, e.KeyStatus.ScanCode, e.KeyStatus.IsExtendedKey);
        var result = capture.Feed(physical, pressed, e.KeyStatus.WasKeyDown);
        Render();
        if (result.Outcome == CaptureOutcome.Completed) Committed?.Invoke(result.Binding!);
        else if (result.Outcome == CaptureOutcome.Cancelled) Cancelled?.Invoke();
        else if (result.Outcome == CaptureOutcome.Unsupported) Unsupported?.Invoke();
    }

    private void Render()
    {
        label.Text = preparing ? Loc.T("key.preparing") : capture.Active
            ? capture.Preview.Length > 0 ? Theme.Keys(capture.Preview) + " …" : Loc.T("key.recordingShort")
            : IsBusy ? Loc.T("key.saving") : Theme.Keys(binding);
        clear.Visibility = binding.Length > 0 && !IsCapturing ? Visibility.Visible : Visibility.Collapsed;
        clear.IsEnabled = !IsBusy;
        View.BorderBrush = new SolidColorBrush(IsCapturing ? Color.FromArgb(255, 0, 103, 192) : Color.FromArgb(255, 190, 190, 190));
        AutomationProperties.SetName(record, title + ": " + label.Text);
    }
}
