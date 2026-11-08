using System.Net;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json.Nodes;
using LiveListen.Services;

if (args.Contains("--diagnose", StringComparer.Ordinal))
{
    var address = new Uri("https://api.github.com/repos/FueTsui/livelisten/releases/latest");
    Console.WriteLine($"ProxyBypassed={HttpClient.DefaultProxy.IsBypassed(address)} ProxyHost={HttpClient.DefaultProxy.GetProxy(address)?.Host}");
    using var client = new HttpClient(new HttpClientHandler { AllowAutoRedirect = false, UseCookies = false }) { Timeout = TimeSpan.FromSeconds(30) };
    client.DefaultRequestHeaders.UserAgent.ParseAdd("LiveListen-Updater/1.0");
    client.DefaultRequestHeaders.Accept.ParseAdd("application/vnd.github+json");
    try
    {
        using var reply = await client.GetAsync(address);
        Console.WriteLine($"HTTP {(int)reply.StatusCode} {reply.Version}");
    }
    catch (Exception exception)
    {
        for (var error = exception; error is not null; error = error.InnerException)
            Console.WriteLine($"Error={error.GetType().Name} HResult={error.HResult}" +
                (error is HttpRequestException request ? $" RequestError={request.HttpRequestError}" : "") +
                (error is System.Net.Sockets.SocketException socket ? $" SocketError={socket.SocketErrorCode}" : ""));
    }
    return;
}

// HTTP fixtures and generated files only; no UI, credentials or user configuration.
var outputRoot = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "update-check-artifacts", Guid.NewGuid().ToString("N")));
Directory.CreateDirectory(outputRoot);
var payload = Encoding.UTF8.GetBytes("A verified fixture package, not executable content.");
var checks = new (string Name, Func<Task> Run)[]
{
    ("numeric versions and installed/portable package selection", Versions),
    ("foreign URLs, malformed hashes and duplicate assets are rejected", InvalidMetadata),
    ("a newer release downloads, verifies and exposes only the completed file", Download),
    ("current version never requests an asset", Current),
    ("hash mismatch removes the partial file and permits retry", HashFailureAndRetry),
    ("truncated and oversized bodies cannot become ready", InvalidBodies),
    ("redirects cannot escape GitHub release storage", Redirects),
    ("unavailable and rate-limited metadata never claims current", HttpErrors),
    ("malformed and oversized metadata remain retryable", MetadataErrors),
    ("concurrent clicks share the active operation", Concurrent),
    ("closed observers cannot interrupt a download", BrokenObserver),
};
foreach (var (name, run) in checks)
{
    await run().WaitAsync(TimeSpan.FromSeconds(10));
    Console.WriteLine("PASS " + name);
}
Console.WriteLine($"{checks.Length} update checks passed");
if (args.Contains("--live", StringComparer.Ordinal))
{
    await UpdateService.Shared.CheckAsync("0.1.0", Path.Combine(outputRoot, "live"), false);
    Console.WriteLine($"LIVE phase={UpdateService.Shared.Status.Phase} version={UpdateService.Shared.Status.Version ?? "none"} error={UpdateService.Shared.Status.Error ?? "none"}");
}

void Check(bool result, string message)
{
    if (!result) throw new InvalidOperationException(message);
}

JsonObject Release(string version = "0.2.0")
{
    JsonObject Asset(bool installed)
    {
        var name = installed ? $"LiveListen-Setup-{version}-x64.exe" : $"LiveListen-{version}-portable-x64.zip";
        return new()
        {
            ["name"] = name, ["state"] = "uploaded", ["size"] = payload.Length,
            ["digest"] = "sha256:" + Convert.ToHexString(SHA256.HashData(payload)).ToLowerInvariant(),
            ["browser_download_url"] = $"https://github.com/FueTsui/livelisten/releases/download/v{version}/{name}",
        };
    }
    return new() { ["tag_name"] = "v" + version, ["draft"] = false, ["prerelease"] = false, ["assets"] = new JsonArray(Asset(false), Asset(true)) };
}

HttpResponseMessage Metadata(JsonObject? json = null) => new(HttpStatusCode.OK) { Content = new StringContent((json ?? Release()).ToJsonString()) };
HttpResponseMessage Body(byte[]? bytes = null) => new(HttpStatusCode.OK) { Content = new ByteArrayContent(bytes ?? payload) };
string DirectoryFor(string name) => Path.Combine(outputRoot, name);
UpdateService Service(Func<HttpRequestMessage, int, Task<HttpResponseMessage>> respond) => new(new HttpClient(new FixtureHandler(respond)));

Task Versions()
{
    var json = Release("0.10.0").ToJsonString();
    Check(UpdateRelease.Parse(json, "0.9.0", false)?.FileName.EndsWith("portable-x64.zip") == true, "numeric comparison or portable selection failed");
    Check(UpdateRelease.Parse(json, "0.9.0", true)?.FileName.StartsWith("LiveListen-Setup-") == true, "installer selection failed");
    Check(UpdateRelease.Parse(json, "0.10.0", false) is null && UpdateRelease.Parse(json, "1.0.0", false) is null, "same or older release offered");
    var preview = Release();
    preview["prerelease"] = true;
    try { UpdateRelease.Parse(preview.ToJsonString(), "0.1.0", false); throw new Exception("preview accepted"); }
    catch (UpdateException error) { Check(error.Code == "unavailable", "wrong preview error"); }
    return Task.CompletedTask;
}

Task InvalidMetadata()
{
    foreach (var change in new Action<JsonObject>[]
    {
        json => json["assets"]![0]!["browser_download_url"] = "https://example.com/package.zip",
        json => json["assets"]![0]!["digest"] = "sha256:bad",
        json => json["assets"]![0]!["size"] = "bad",
        json => json["assets"]![0]!["size"] = -1,
        json => json["assets"]!.AsArray().Add(json["assets"]![0]!.DeepClone()),
        json => json["tag_name"] = "v0.2.0/../../elsewhere",
        json => json["tag_name"] = "vv0.2.0",
    })
    {
        var json = Release();
        change(json);
        try { UpdateRelease.Parse(json.ToJsonString(), "0.1.0", false); throw new Exception("invalid metadata accepted"); }
        catch (UpdateException) { }
    }
    return Task.CompletedTask;
}

async Task Download()
{
    var service = Service((_, index) => Task.FromResult(index == 1 ? Metadata() : Body()));
    var phases = new List<UpdatePhase>();
    service.Changed += status => phases.Add(status.Phase);
    await service.CheckAsync("0.1.0", DirectoryFor("success"), false);
    Check(service.Status.Phase == UpdatePhase.Ready && service.Status.Version == "0.2.0", "successful download not ready");
    Check(File.ReadAllBytes(service.Status.DownloadPath!).SequenceEqual(payload), "downloaded bytes differ");
    Check(phases.Contains(UpdatePhase.Downloading) && phases.Contains(UpdatePhase.Verifying), "missing progress or verification");
    Check(!Directory.EnumerateFiles(DirectoryFor("success"), "*.partial", SearchOption.AllDirectories).Any(), "partial remains after success");
}

async Task Current()
{
    var requests = 0;
    var service = Service((_, _) => { requests++; return Task.FromResult(Metadata(Release("0.1.0"))); });
    await service.CheckAsync("0.1.0", DirectoryFor("current"), false);
    Check(service.Status.Phase == UpdatePhase.Current && requests == 1 && !Directory.Exists(DirectoryFor("current")), "current release downloaded anything");
}

async Task HashFailureAndRetry()
{
    var corrupt = true;
    var service = Service((_, index) => Task.FromResult(index % 2 == 1 ? Metadata() : Body(corrupt ? new byte[payload.Length] : payload)));
    await service.CheckAsync("0.1.0", DirectoryFor("hash"), false);
    Check(service.Status is { Phase: UpdatePhase.Failed, Error: "integrity", DownloadPath: null }, "hash mismatch was accepted");
    Check(!Directory.EnumerateFiles(DirectoryFor("hash"), "*", SearchOption.AllDirectories).Any(), "failed package retained");
    corrupt = false;
    await service.CheckAsync("0.1.0", DirectoryFor("hash"), false);
    Check(service.Status.Phase == UpdatePhase.Ready, "failure prevented retry");
}

async Task InvalidBodies()
{
    foreach (var length in new[] { payload.Length - 1, payload.Length + 1 })
    {
        foreach (var chunked in new[] { false, true })
        {
            var service = Service((_, index) => Task.FromResult(index == 1 ? Metadata() : chunked
                ? new HttpResponseMessage(HttpStatusCode.OK) { Content = new ChunkedBody(new byte[length]) } : Body(new byte[length])));
            var directory = DirectoryFor("body-" + length + "-" + chunked);
            await service.CheckAsync("0.1.0", directory, false);
            Check(service.Status is { Phase: UpdatePhase.Failed, Error: "integrity", DownloadPath: null }, "invalid body became ready");
            Check(!Directory.Exists(directory) || !Directory.EnumerateFiles(directory, "*", SearchOption.AllDirectories).Any(), "partial invalid body retained");
        }
    }
}

async Task Redirects()
{
    var requests = 0;
    var service = Service((_, index) =>
    {
        requests++;
        if (index == 1) return Task.FromResult(Metadata());
        var redirect = new HttpResponseMessage(HttpStatusCode.Redirect);
        redirect.Headers.Location = new Uri("https://example.com/package");
        return Task.FromResult(redirect);
    });
    await service.CheckAsync("0.1.0", DirectoryFor("redirect"), false);
    Check(service.Status is { Phase: UpdatePhase.Failed, Error: "metadata" } && requests == 2, "unsafe redirect followed");
    Check(UpdateRelease.IsAssetRedirect(new Uri("https://release-assets.githubusercontent.com/asset?id=example")), "official CDN refused");
    Check(!UpdateRelease.IsAssetRedirect(new Uri("http://release-assets.githubusercontent.com/asset")), "insecure redirect allowed");
    Check(!UpdateRelease.IsAssetRedirect(new Uri("https://release-assets.githubusercontent.com.example.com/asset")), "spoofed host allowed");
    var official = Service((request, index) =>
    {
        if (index == 1) return Task.FromResult(Metadata());
        if (index == 2)
        {
            var redirect = new HttpResponseMessage(HttpStatusCode.Redirect);
            redirect.Headers.Location = new Uri("https://release-assets.githubusercontent.com/fixture-asset");
            return Task.FromResult(redirect);
        }
        Check(request.RequestUri?.Host == "release-assets.githubusercontent.com", "wrong redirect destination");
        return Task.FromResult(Body());
    });
    await official.CheckAsync("0.1.0", DirectoryFor("official-redirect"), false);
    Check(official.Status.Phase == UpdatePhase.Ready, "official release redirect failed");
}

async Task HttpErrors()
{
    foreach (var (status, expected) in new[] { (HttpStatusCode.NotFound, "unavailable"), (HttpStatusCode.TooManyRequests, "rateLimit"), (HttpStatusCode.InternalServerError, "network") })
    {
        var service = Service((_, _) => Task.FromResult(new HttpResponseMessage(status)));
        await service.CheckAsync("0.1.0", DirectoryFor("http-" + (int)status), false);
        Check(service.Status.Phase == UpdatePhase.Failed && service.Status.Error == expected, "HTTP failure claimed current");
    }
}

async Task MetadataErrors()
{
    foreach (var content in new[] { "{broken", new string('x', 2 * 1024 * 1024 + 1) })
    {
        var service = Service((_, _) => Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK) { Content = new StringContent(content) }));
        await service.CheckAsync("0.1.0", DirectoryFor("metadata"), false);
        Check(service.Status is { Phase: UpdatePhase.Failed, Error: "metadata" }, "malformed metadata escaped error handling");
    }
}

async Task Concurrent()
{
    var blocked = new TaskCompletionSource<HttpResponseMessage>(TaskCreationOptions.RunContinuationsAsynchronously);
    var requests = 0;
    var service = Service((_, _) => { requests++; return blocked.Task; });
    var first = service.CheckAsync("0.1.0", DirectoryFor("concurrent"), false);
    await service.CheckAsync("0.1.0", DirectoryFor("concurrent"), false);
    Check(requests == 1, "concurrent checks started multiple requests");
    blocked.SetResult(Metadata(Release("0.1.0")));
    await first;
    Check(service.Status.Phase == UpdatePhase.Current, "accepted check did not complete");
}

async Task BrokenObserver()
{
    var service = Service((_, index) => Task.FromResult(index == 1 ? Metadata() : Body()));
    service.Changed += _ => throw new InvalidOperationException("closed view");
    await service.CheckAsync("0.1.0", DirectoryFor("observer"), true);
    Check(service.Status.Phase == UpdatePhase.Ready, "view exception interrupted download");
}

sealed class FixtureHandler(Func<HttpRequestMessage, int, Task<HttpResponseMessage>> respond) : HttpMessageHandler
{
    private int count;
    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token) =>
        respond(request, Interlocked.Increment(ref count));
}

sealed class ChunkedBody(byte[] bytes) : HttpContent
{
    protected override Task SerializeToStreamAsync(Stream stream, TransportContext? context) => stream.WriteAsync(bytes).AsTask();
    protected override bool TryComputeLength(out long length) { length = 0; return false; }
}
