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
    ("daily checks request metadata only and persist across restarts", DailyChecks),
    ("manual installation waits for the daily query and runs once", ManualDuringDailyCheck),
    ("portable and corrupted packages are never executed", NeverInstallUnverifiedOrPortable),
    ("an installer failure exposes the verified package for diagnosis", InstallerFailure),
    ("proxy modes select the system, no or a custom proxy and refuse unusable addresses", ProxyModes),
    ("a failed daily check stays silent and a later manual check still works", DailyFailureThenManual),
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

async Task DailyChecks()
{
    var directory = DirectoryFor("daily");
    var day = new DateOnly(2026, 11, 9);
    var requests = 0;
    var service = Service((_, index) =>
    {
        requests++;
        if (index != 1) throw new InvalidOperationException("daily check requested an asset");
        return Task.FromResult(Metadata());
    });
    Check(await service.CheckDailyAsync("0.1.0", directory, true, day) == "0.2.0", "missing daily notice");
    Check(service.Status is { Phase: UpdatePhase.Available, Version: "0.2.0", DownloadPath: null }, "daily check became a download");
    Check(requests == 1 && !Directory.EnumerateDirectories(directory).Any(), "daily check downloaded a package");

    var restarted = Service((_, _) => { requests++; return Task.FromResult(new HttpResponseMessage(HttpStatusCode.InternalServerError)); });
    Check(await restarted.CheckDailyAsync("0.1.0", directory, true, day) is null && requests == 1, "restart checked again");
    Check(await restarted.CheckDailyAsync("0.1.0", directory, true, day.AddDays(1)) is null, "failed query announced an update");
    Check(restarted.Status.Phase == UpdatePhase.Idle, "background failure changed the visible status");
    Check(await restarted.CheckDailyAsync("0.1.0", directory, true, day.AddDays(1)) is null && requests == 2,
        "background failure was retried on the same day");
}

async Task ManualDuringDailyCheck()
{
    var directory = DirectoryFor("daily-then-install");
    var entered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
    var response = new TaskCompletionSource<HttpResponseMessage>(TaskCreationOptions.RunContinuationsAsynchronously);
    var service = Service((request, index) =>
    {
        if (index == 1) { entered.SetResult(); return response.Task; }
        if (index == 2) return Task.FromResult(Metadata());
        Check(index == 3 && request.RequestUri!.AbsolutePath.EndsWith("LiveListen-Setup-0.2.0-x64.exe"), "wrong download");
        return Task.FromResult(Body());
    });
    var installed = 0;
    Task Install(string path, CancellationToken _)
    {
        Check(service.Status is { Phase: UpdatePhase.Installing, Version: "0.2.0" }, "installer ran without an installing state");
        Check(File.ReadAllBytes(path).SequenceEqual(payload), "installer received unverified bytes");
        installed++;
        return Task.CompletedTask;
    }

    var daily = service.CheckDailyAsync("0.1.0", directory, true, new DateOnly(2026, 11, 9));
    await entered.Task;
    var manual = service.CheckAsync("0.1.0", directory, true, install: Install);
    Check(!manual.IsCompleted, "manual click was lost while the daily check was active");
    await service.CheckAsync("0.1.0", directory, true, install: Install);
    response.SetResult(Metadata());
    Check(await daily is null, "daily check announced a release already requested by the user");
    await manual;
    Check(installed == 1 && service.Status.Phase == UpdatePhase.Current, "verified installer was not run exactly once");
}

async Task NeverInstallUnverifiedOrPortable()
{
    static Task Install(string _, CancellationToken __) => throw new InvalidOperationException("unexpected execution");
    var portable = Service((_, index) => Task.FromResult(index == 1 ? Metadata() : Body()));
    await portable.CheckAsync("0.1.0", DirectoryFor("portable-no-install"), false, install: Install);
    Check(portable.Status.Phase == UpdatePhase.Ready, "portable update did not remain downloadable");

    var corrupt = Service((_, index) => Task.FromResult(index == 1 ? Metadata() : Body(new byte[payload.Length])));
    await corrupt.CheckAsync("0.1.0", DirectoryFor("invalid-no-install"), true, install: Install);
    Check(corrupt.Status is { Phase: UpdatePhase.Failed, Error: "integrity", DownloadPath: null }, "corrupt installer became ready");
}

Task ProxyModes()
{
    using var system = UpdateService.CreateHandler("system", "");
    Check(system.UseProxy && system.Proxy is null && !system.AllowAutoRedirect, "system mode must keep the Windows proxy settings");
    using var direct = UpdateService.CreateHandler("direct", "http://127.0.0.1:7890");
    Check(!direct.UseProxy && direct.Proxy is null, "direct mode still uses a proxy");
    using var custom = UpdateService.CreateHandler("custom", " http://127.0.0.1:7890 ");
    var target = new Uri("https://api.github.com/");
    Check(custom.UseProxy && custom.Proxy?.GetProxy(target) is { Host: "127.0.0.1", Port: 7890 }, "custom proxy address was not applied");
    foreach (var bad in new[] { "", "127.0.0.1:7890", "ftp://proxy.example:21", "socks5://proxy.example:1080", "http://user:secret@proxy.example:8080",
        "http://proxy.example:8080/path", "http://", "not a url" })
    {
        Check(UpdateService.ProxyUri(bad) is null, "unusable proxy address was accepted: " + bad);
        using var fallback = UpdateService.CreateHandler("custom", bad);
        Check(fallback.UseProxy && fallback.Proxy is null, "unusable proxy address did not fall back to the system settings");
    }
    using var unknown = UpdateService.CreateHandler("something-else", "http://127.0.0.1:1");
    Check(unknown.UseProxy && unknown.Proxy is null, "unknown mode did not behave like system");
    return Task.CompletedTask;
}

async Task DailyFailureThenManual()
{
    var directory = DirectoryFor("daily-failure");
    var calls = 0;
    var service = Service((_, _) =>
    {
        calls++;
        if (calls == 1) throw new HttpRequestException("offline");
        return Task.FromResult(calls == 2 ? Metadata() : Body());
    });
    var today = new DateOnly(2026, 11, 11);
    Check(await service.CheckDailyAsync("0.1.0", directory, false, today) is null && service.Status.Phase == UpdatePhase.Idle,
        "a failed daily check surfaced an error or a version");
    Check(await service.CheckDailyAsync("0.1.0", directory, false, today) is null && calls == 1,
        "a failed daily check was repeated on the same day");
    await service.CheckAsync("0.1.0", directory, false);
    Check(service.Status.Phase == UpdatePhase.Ready && calls == 3, "a manual check after a failed daily check did not complete");
}

async Task InstallerFailure()
{
    var service = Service((_, index) => Task.FromResult(index == 1 ? Metadata() : Body()));
    await service.CheckAsync("0.1.0", DirectoryFor("install-failed"), true,
        install: (_, _) => throw new UpdateException("install"));
    Check(service.Status is { Phase: UpdatePhase.Failed, Error: "install", DownloadPath: not null } &&
        File.Exists(service.Status.DownloadPath), "installer failure was hidden or lost its verified package");
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
