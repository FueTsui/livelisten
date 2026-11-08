using Microsoft.Win32;

namespace LiveListen.Services;

/// <summary>Start with Windows through the per-user Run key; no admin rights needed.</summary>
internal static class Startup
{
    private const string RunKey = @"Software\Microsoft\Windows\CurrentVersion\Run";
    private const string ValueName = "LiveListen";

    public static string Command => $"\"{Environment.ProcessPath}\" --background";

    public static bool IsRegistered()
    {
        using var key = Registry.CurrentUser.OpenSubKey(RunKey);
        return key?.GetValue(ValueName) is string;
    }

    /// <summary>True when the Run entry starts exactly this program in the background.</summary>
    public static bool PointsHere()
    {
        using var key = Registry.CurrentUser.OpenSubKey(RunKey);
        return key?.GetValue(ValueName) is string value && value.Equals(Command, StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>Writes or removes the Run entry; returns false when the registry refused.</summary>
    public static bool Apply(bool on)
    {
        try
        {
            using var key = Registry.CurrentUser.CreateSubKey(RunKey, writable: true);
            if (on)
            {
                key.SetValue(ValueName, Command);
            }
            else
            {
                key.DeleteValue(ValueName, throwOnMissingValue: false);
            }
            return true;
        }
        catch (Exception e) when (e is UnauthorizedAccessException or System.Security.SecurityException or IOException)
        {
            return false;
        }
    }

    /// <summary>Keeps the Run entry in step with the setting, e.g. after the app was moved.</summary>
    public static void Sync(bool on)
    {
        if (on)
        {
            Apply(true);
        }
        else if (IsRegistered())
        {
            Apply(false);
        }
    }
}
