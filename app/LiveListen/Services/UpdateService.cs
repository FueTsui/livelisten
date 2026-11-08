using System.Net;
using System.Net.Http;
using System.Security.Cryptography;
using System.Text.Json;

namespace LiveListen.Services;

internal enum UpdatePhase { Idle, Checking, Current, Downloading, Verifying, Ready, Failed }

internal sealed record UpdateStatus(UpdatePhase Phase, string? Version = null, int Percent = 0,
    string? DownloadPath = null, string? Error = null);

/// <summary>Downloads verified release assets without executing them or replacing an installation.</summary>
internal sealed class UpdateService
{
    private static readonly Uri Latest = new("https://api.github.com/repos/FueTsui/livelisten/releases/latest");
    private const int MetadataLimit = 2 * 1024 * 1024;
    private readonly HttpClient client;
    private readonly SemaphoreSlim gate = new(1, 1);

    public static UpdateService Shared { get; } = new(new HttpClient(new HttpClientHandler
    {
        AllowAutoRedirect = false,
        UseCookies = false,
    }) { Timeout = Timeout.InfiniteTimeSpan });

    public UpdateService(HttpClient client) => this.client = client;
    public UpdateStatus Status { get; private set; } = new(UpdatePhase.Idle);
    public event Action<UpdateStatus>? Changed;

    public async Task CheckAsync(string currentVersion, string downloadDirectory, bool installed,
        CancellationToken cancellationToken = default)
    {
        if (!await gate.WaitAsync(0, cancellationToken).ConfigureAwait(false)) return;
        string? partial = null;
        string? version = null;
        try
        {
            Set(new(UpdatePhase.Checking));
            using var metadataTimeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            metadataTimeout.CancelAfter(TimeSpan.FromSeconds(30));
            using var response = await GetAsync(Latest, metadataTimeout.Token).ConfigureAwait(false);
            if (response.StatusCode == HttpStatusCode.NotFound) throw new UpdateException("unavailable");
            CheckResponse(response);
            var json = await ReadLimitedAsync(response, MetadataLimit, metadataTimeout.Token).ConfigureAwait(false);
            var release = UpdateRelease.Parse(json, currentVersion, installed);
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
            Set(new(UpdatePhase.Ready, version, 100, destination));
        }
        catch (UpdateException error) { Set(new(UpdatePhase.Failed, version, Error: error.Code)); }
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
            gate.Release();
        }
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
