using System.Globalization;
using System.Net;
using System.Net.Http;
using System.Security.Cryptography;
using System.Text.Json;

namespace LiveListen.Services;

internal enum UpdatePhase { Idle, Checking, Available, Current, Downloading, Verifying, Ready, Installing, Failed }

internal sealed record UpdateStatus(UpdatePhase Phase, string? Version = null, int Percent = 0,
    string? DownloadPath = null, string? Error = null);

/// <summary>Checks for releases and hands off verified installer assets only on a manual request.</summary>
internal sealed class UpdateService
{
    private static readonly Uri Latest = new("https://api.github.com/repos/FueTsui/livelisten/releases/latest");
    private const int MetadataLimit = 2 * 1024 * 1024;
    private HttpClient client;
    private (string Mode, string Url) proxy = ("system", "");
    private readonly SemaphoreSlim gate = new(1, 1);
    private int manualCheck;

    public static UpdateService Shared { get; } = new(NewClient("system", ""));

    private static HttpClient NewClient(string mode, string url) =>
        new(CreateHandler(mode, url)) { Timeout = Timeout.InfiniteTimeSpan };

    /// <summary>
    /// `system` follows the Windows proxy settings, `direct` uses none, `custom` uses the
    /// given HTTP proxy. An unusable custom address falls back to the system settings. The
    /// core applies the same choice to the speech service processes.
    /// </summary>
    internal static HttpClientHandler CreateHandler(string mode, string url)
    {
        var handler = new HttpClientHandler { AllowAutoRedirect = false, UseCookies = false };
        if (mode == "direct") handler.UseProxy = false;
        else if (mode == "custom" && ProxyUri(url) is { } address)
        {
            handler.Proxy = new WebProxy(address);
            handler.UseProxy = true;
        }
        return handler;
    }

    /// <summary>A proxy address the app accepts, or null. Credentials in the address are refused.</summary>
    internal static Uri? ProxyUri(string? text) =>
        Uri.TryCreate(text?.Trim(), UriKind.Absolute, out var uri) && uri.Host.Length > 0 && uri.UserInfo.Length == 0
            && uri.Scheme is "http" or "https" && uri.AbsolutePath is "/" or "" && uri.Query.Length == 0
            ? uri : null;

    /// <summary>Uses the proxy for the next request; a running download keeps its connection.</summary>
    public void SetProxy(string mode, string url)
    {
        mode = mode is "direct" or "custom" ? mode : "system";
        url = url.Trim();
        if (proxy == (mode, url)) return;
        proxy = (mode, url);
        client = NewClient(mode, url);
    }

    public UpdateService(HttpClient client) => this.client = client;
    public UpdateStatus Status { get; private set; } = new(UpdatePhase.Idle);
    public event Action<UpdateStatus>? Changed;

    /// <summary>Checks metadata at most once per local day, across app restarts.</summary>
    public async Task<string?> CheckDailyAsync(string currentVersion, string downloadDirectory, bool installed, DateOnly localDate)
    {
        if (Volatile.Read(ref manualCheck) != 0 || !await gate.WaitAsync(0).ConfigureAwait(false)) return null;
        try
        {
            var today = localDate.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture);
            var stamp = Path.Combine(downloadDirectory, "last-daily-check.txt");
            if (File.Exists(stamp) && await File.ReadAllTextAsync(stamp).ConfigureAwait(false) == today) return null;
            // Record the attempt before making a request so failures don't trigger repeated checks.
            Directory.CreateDirectory(downloadDirectory);
            await File.WriteAllTextAsync(stamp, today).ConfigureAwait(false);

            var release = await LatestReleaseAsync(currentVersion, installed, CancellationToken.None).ConfigureAwait(false);
            if (Volatile.Read(ref manualCheck) != 0) return null;
            if (release is not null && Status.Phase is UpdatePhase.Idle or UpdatePhase.Current or UpdatePhase.Available)
                Set(new(UpdatePhase.Available, release.Version));
            else if (release is null && Status.Phase == UpdatePhase.Available)
                Set(new(UpdatePhase.Idle));
            return release?.Version;
        }
        catch (Exception error) when (error is UpdateException or OperationCanceledException or HttpRequestException
            or JsonException or IOException or UnauthorizedAccessException)
        {
            // Background failures are silent. A manual check can retry and show the reason.
            return null;
        }
        finally { gate.Release(); }
    }

    public async Task CheckAsync(string currentVersion, string downloadDirectory, bool installed,
        CancellationToken cancellationToken = default, Func<string, CancellationToken, Task>? install = null)
    {
        if (Interlocked.Exchange(ref manualCheck, 1) != 0) return;
        var acquired = false;
        string? partial = null;
        string? version = null;
        string? verified = null;
        try
        {
            await gate.WaitAsync(cancellationToken).ConfigureAwait(false);
            acquired = true;
            Set(new(UpdatePhase.Checking));
            var release = await LatestReleaseAsync(currentVersion, installed, cancellationToken).ConfigureAwait(false);
            if (release is null)
            {
                Set(new(UpdatePhase.Current));
                return;
            }
            version = release.Version;
            Set(new(UpdatePhase.Downloading, version));
            using var downloadTimeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            downloadTimeout.CancelAfter(TimeSpan.FromMinutes(15));
            using var download = await GetAsync(release.Url, downloadTimeout.Token).ConfigureAwait(false);
            CheckResponse(download);
            if (download.Content.Headers.ContentLength is long length && length != release.Size)
                throw new UpdateException("integrity");

            // Each attempt has its own directory; a failed download never overwrites a verified file.
            var directory = Path.Combine(downloadDirectory, version + "-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(directory);
            var destination = Path.Combine(directory, release.FileName);
            partial = destination + ".partial";
            using var hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
            await using (var input = await download.Content.ReadAsStreamAsync(downloadTimeout.Token).ConfigureAwait(false))
            await using (var output = new FileStream(partial, FileMode.CreateNew, FileAccess.Write, FileShare.None,
                81920, FileOptions.Asynchronous | FileOptions.SequentialScan))
            {
                var buffer = new byte[81920];
                long received = 0;
                var previousPercent = -1;
                int count;
                while ((count = await input.ReadAsync(buffer, downloadTimeout.Token).ConfigureAwait(false)) != 0)
                {
                    received += count;
                    if (received > release.Size) throw new UpdateException("integrity");
                    hash.AppendData(buffer, 0, count);
                    await output.WriteAsync(buffer.AsMemory(0, count), downloadTimeout.Token).ConfigureAwait(false);
                    var percent = (int)(received * 100 / release.Size);
                    if (percent != previousPercent)
                    {
                        previousPercent = percent;
                        Set(new(UpdatePhase.Downloading, version, percent));
                    }
                }
                if (received != release.Size) throw new UpdateException("integrity");
                await output.FlushAsync(downloadTimeout.Token).ConfigureAwait(false);
            }
            Set(new(UpdatePhase.Verifying, version, 100));
            if (!CryptographicOperations.FixedTimeEquals(hash.GetHashAndReset(), Convert.FromHexString(release.Sha256)))
                throw new UpdateException("integrity");
            File.Move(partial, destination);
            partial = null;
            if (installed && install is not null)
            {
                // Verify the bytes on disk and hold the executable open without write sharing
                // until the installer has finished. It can still read its appended payload.
                await using var executable = new FileStream(destination, FileMode.Open, FileAccess.Read, FileShare.Read,
                    81920, FileOptions.Asynchronous | FileOptions.SequentialScan);
                var digest = await SHA256.HashDataAsync(executable, cancellationToken).ConfigureAwait(false);
                if (!CryptographicOperations.FixedTimeEquals(digest, Convert.FromHexString(release.Sha256)))
                    throw new UpdateException("integrity");
                verified = destination;
                Set(new(UpdatePhase.Installing, version, 100));
                await install(destination, cancellationToken).ConfigureAwait(false);
                Set(new(UpdatePhase.Current));
            }
            else Set(new(UpdatePhase.Ready, version, 100, destination));
        }
        catch (UpdateException error) { Set(new(UpdatePhase.Failed, version, DownloadPath: verified, Error: error.Code)); }
        catch (OperationCanceledException) { Set(new(UpdatePhase.Failed, version, Error: "timeout")); }
        catch (HttpRequestException) { Set(new(UpdatePhase.Failed, version, Error: "network")); }
        catch (HttpIOException) { Set(new(UpdatePhase.Failed, version, Error: "network")); }
        catch (JsonException) { Set(new(UpdatePhase.Failed, version, Error: "metadata")); }
        catch (IOException) { Set(new(UpdatePhase.Failed, version, Error: "storage")); }
        catch (UnauthorizedAccessException) { Set(new(UpdatePhase.Failed, version, Error: "storage")); }
        finally
        {
            if (partial is not null)
            {
                try { File.Delete(partial); }
                catch (IOException) { }
                catch (UnauthorizedAccessException) { }
            }
            if (acquired) gate.Release();
            Volatile.Write(ref manualCheck, 0);
        }
    }

    private async Task<UpdateRelease?> LatestReleaseAsync(string currentVersion, bool installed, CancellationToken token)
    {
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(token);
        timeout.CancelAfter(TimeSpan.FromSeconds(30));
        using var response = await GetAsync(Latest, timeout.Token).ConfigureAwait(false);
        if (response.StatusCode == HttpStatusCode.NotFound) throw new UpdateException("unavailable");
        CheckResponse(response);
        var json = await ReadLimitedAsync(response, MetadataLimit, timeout.Token).ConfigureAwait(false);
        return UpdateRelease.Parse(json, currentVersion, installed);
    }

    private void Set(UpdateStatus status)
    {
        Status = status;
        // A closing settings window must not interrupt the verified download.
        foreach (var observer in Changed?.GetInvocationList() ?? [])
        {
            try { ((Action<UpdateStatus>)observer)(status); }
            catch (Exception) { }
        }
    }

    private async Task<HttpResponseMessage> GetAsync(Uri uri, CancellationToken token)
    {
        for (var redirects = 0; redirects <= 4; redirects++)
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, uri);
            request.Headers.UserAgent.ParseAdd("LiveListen-Updater/1.0");
            request.Headers.Accept.ParseAdd(uri.Host == "api.github.com" ? "application/vnd.github+json" : "application/octet-stream");
            using var headerTimeout = CancellationTokenSource.CreateLinkedTokenSource(token);
            headerTimeout.CancelAfter(TimeSpan.FromSeconds(30));
            var response = await client.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, headerTimeout.Token).ConfigureAwait(false);
            if ((int)response.StatusCode is not (301 or 302 or 303 or 307 or 308)) return response;
            var location = response.Headers.Location;
            response.Dispose();
            if (location is null) throw new UpdateException("metadata");
            var next = location.IsAbsoluteUri ? location : new Uri(uri, location);
            // Only GitHub's release storage may receive asset redirects; never forward credentials.
            if (uri.Host == "api.github.com" || !UpdateRelease.IsAssetRedirect(next))
                throw new UpdateException("metadata");
            uri = next;
        }
        throw new UpdateException("network");
    }

    private static void CheckResponse(HttpResponseMessage response)
    {
        if (response.StatusCode is HttpStatusCode.Forbidden or HttpStatusCode.TooManyRequests)
            throw new UpdateException("rateLimit");
        if (!response.IsSuccessStatusCode) throw new UpdateException("network");
    }

    private static async Task<string> ReadLimitedAsync(HttpResponseMessage response, int limit, CancellationToken token)
    {
        if (response.Content.Headers.ContentLength is long length && length > limit)
            throw new UpdateException("metadata");
        await using var input = await response.Content.ReadAsStreamAsync(token).ConfigureAwait(false);
        using var output = new MemoryStream();
        var buffer = new byte[8192];
        int count;
        while ((count = await input.ReadAsync(buffer, token).ConfigureAwait(false)) != 0)
        {
            if (output.Length + count > limit) throw new UpdateException("metadata");
            output.Write(buffer, 0, count);
        }
        return System.Text.Encoding.UTF8.GetString(output.ToArray());
    }
}

internal sealed class UpdateException(string code) : Exception(code)
{
    public string Code { get; } = code;
}
