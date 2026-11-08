using System.Text.Json;
using System.Text.RegularExpressions;

namespace LiveListen.Services;

internal sealed record UpdateRelease(string Version, string FileName, Uri Url, long Size, string Sha256)
{
    private const long MaximumPackageSize = 2L * 1024 * 1024 * 1024;

    public static UpdateRelease? Parse(string json, string currentVersion, bool installed)
    {
        using var document = JsonDocument.Parse(json);
        var root = document.RootElement;
        if (root.ValueKind != JsonValueKind.Object || !TryVersion(currentVersion, out var current))
            throw new UpdateException("metadata");
        if (!root.TryGetProperty("draft", out var draft) || draft.ValueKind != JsonValueKind.False ||
            !root.TryGetProperty("prerelease", out var prerelease) || prerelease.ValueKind != JsonValueKind.False)
            throw new UpdateException("unavailable");
        var tag = String(root, "tag_name");
        if (!TryVersion(tag, out var available)) throw new UpdateException("metadata");
        if (available <= current) return null;
        var version = available.ToString(3);
        var expectedName = installed ? $"LiveListen-Setup-{version}-x64.exe" : $"LiveListen-{version}-portable-x64.zip";
        if (!root.TryGetProperty("assets", out var assets) || assets.ValueKind != JsonValueKind.Array)
            throw new UpdateException("metadata");
        var matches = assets.EnumerateArray().Where(asset => String(asset, "name") == expectedName).ToArray();
        if (matches.Length != 1) throw new UpdateException("asset");
        var selected = matches[0];
        var expectedUrl = new Uri($"https://github.com/FueTsui/livelisten/releases/download/{tag}/{expectedName}");
        if (!Uri.TryCreate(String(selected, "browser_download_url"), UriKind.Absolute, out var url) ||
            !string.Equals(url.AbsoluteUri, expectedUrl.AbsoluteUri, StringComparison.Ordinal) ||
            !selected.TryGetProperty("size", out var sizeValue) || sizeValue.ValueKind != JsonValueKind.Number || !sizeValue.TryGetInt64(out var size) ||
            size <= 0 || size > MaximumPackageSize || String(selected, "state") != "uploaded")
            throw new UpdateException("metadata");
        var digest = String(selected, "digest");
        if (!Regex.IsMatch(digest, "^sha256:[a-fA-F0-9]{64}$", RegexOptions.CultureInvariant))
            throw new UpdateException("integrity");
        return new(version, expectedName, url, size, digest[7..]);
    }

    internal static bool IsAssetRedirect(Uri uri) =>
        uri.Scheme == "https" && uri.IsDefaultPort && uri.UserInfo.Length == 0 && uri.Fragment.Length == 0 &&
        uri.Host is "release-assets.githubusercontent.com" or "objects.githubusercontent.com";

    private static bool TryVersion(string value, out Version version)
    {
        if (value.StartsWith('v')) value = value[1..];
        if (!Regex.IsMatch(value, @"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$", RegexOptions.CultureInvariant) ||
            !System.Version.TryParse(value, out var parsed))
        {
            version = new Version();
            return false;
        }
        version = parsed;
        return true;
    }

    private static string String(JsonElement element, string key) =>
        element.ValueKind == JsonValueKind.Object && element.TryGetProperty(key, out var value) && value.ValueKind == JsonValueKind.String
            ? value.GetString() ?? "" : "";
}
