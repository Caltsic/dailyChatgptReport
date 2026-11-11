using System.Net;
using System.Net.Http.Headers;
using System.Net.Http.Json;
using System.Security.Cryptography;
using System.Text;
using DailyReport.Core;

namespace DailyReport.Client;

public sealed class ApiException(int status, string message) : Exception(message)
{
    public int Status { get; } = status;
    public bool Retryable => Status is 408 or 429 || Status >= 500;
}

public sealed class MemberConnection : IDisposable
{
    readonly HttpClient http;
    public MemberConnection(string url, string fingerprint, string? token = null)
    {
        var uri = new Uri(url);
        if (uri.Scheme != "https" || !string.IsNullOrEmpty(uri.UserInfo) || uri.AbsolutePath != "/") throw new InvalidDataException("部门地址必须是 HTTPS 根地址");
        var pin = fingerprint.Replace(":", "").ToUpperInvariant();
        if (pin.Length != 64 || !pin.All(Uri.IsHexDigit)) throw new InvalidDataException("证书指纹无效");
        var handler = new SocketsHttpHandler { AllowAutoRedirect = false, UseProxy = false, ConnectTimeout = TimeSpan.FromSeconds(5) };
        handler.SslOptions.RemoteCertificateValidationCallback = (_, cert, _, _) => cert is System.Security.Cryptography.X509Certificates.X509Certificate2 peer && DateTime.UtcNow >= peer.NotBefore.ToUniversalTime() && DateTime.UtcNow <= peer.NotAfter.ToUniversalTime() && CryptographicOperations.FixedTimeEquals(Encoding.ASCII.GetBytes(peer.GetCertHashString(HashAlgorithmName.SHA256)), Encoding.ASCII.GetBytes(pin));
        http = new HttpClient(handler) { BaseAddress = uri, Timeout = TimeSpan.FromMinutes(2) };
        if (token != null) http.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Bearer", token);
    }
    public Task<PairReply> Pair(PairRequest request, CancellationToken ct) => Post<PairReply>("/v1/pair", request, ct);
    public Task<Receipt> Submit(Upload request, CancellationToken ct) => Post<Receipt>("/v1/submissions", request, ct);
    public Task<MemberStatus> Status(string day, CancellationToken ct) => Get<MemberStatus>("/v1/status/" + day, ct);
    public Task<MemberStatus> Feedback(SupplementRequest request, CancellationToken ct) => Post<MemberStatus>("/v1/supplements", request, ct);
    public async Task<byte[]> Pdf(string id, CancellationToken ct)
    {
        using var res = await http.GetAsync("/v1/reports/" + Uri.EscapeDataString(id) + "/pdf", ct);
        await Check(res, ct); return await res.Content.ReadAsByteArrayAsync(ct);
    }
    async Task<T> Post<T>(string route, object body, CancellationToken ct)
    {
        using var res = await http.PostAsJsonAsync(route, body, Wire.Json, ct);
        await Check(res, ct); return await res.Content.ReadFromJsonAsync<T>(Wire.Json, ct) ?? throw new InvalidDataException("服务器返回空数据");
    }
    async Task<T> Get<T>(string route, CancellationToken ct)
    {
        using var res = await http.GetAsync(route, ct); await Check(res, ct);
        return await res.Content.ReadFromJsonAsync<T>(Wire.Json, ct) ?? throw new InvalidDataException("服务器返回空数据");
    }
    static async Task Check(HttpResponseMessage res, CancellationToken ct)
    {
        if (res.IsSuccessStatusCode) return;
        var message = $"请求失败（HTTP {(int)res.StatusCode}）";
        try { var error = await res.Content.ReadFromJsonAsync<Dictionary<string, string>>(Wire.Json, ct); if (error?.TryGetValue("error", out var text) == true) message = text; } catch (System.Text.Json.JsonException) { }
        throw new ApiException((int)res.StatusCode, message);
    }
    public void Dispose() => http.Dispose();
}

public record OutboxItem(string Id, DateTimeOffset CreatedAt, string Kind, string Payload, string? Error = null);
public sealed class MemberRepository
{
    public string Root { get; }
    readonly object gate = new();
    public MemberRepository(string root) { Root = root; Directory.CreateDirectory(Path.Combine(root, "outbox")); }
    public Pairing? Pairing => File.Exists(Path.Combine(Root, "pairing.secret")) ? Wire.Parse<Pairing>(LocalFiles.ReadSecret(Path.Combine(Root, "pairing.secret"))) : null;
    public void Bind(Pairing pairing)
    {
        lock (gate)
        {
            var previous = Pairing;
            if (previous != null && (previous.Identity.MemberId != pairing.Identity.MemberId || previous.Identity.DepartmentId != pairing.Identity.DepartmentId))
                throw new InvalidOperationException("此端已绑定其他成员；请使用独立的数据目录，避免混入旧队列");
            LocalFiles.SaveSecret(Path.Combine(Root, "pairing.secret"), Wire.Serialize(pairing));
            // Revoked credentials are repaired by pairing the same identity again.
            foreach (var item in Items().Where(x => x.Error?.StartsWith("HTTP 401") == true)) Save(item with { Error = null });
        }
    }
    public OutboxItem Enqueue(Upload upload) => Add("upload", Wire.Serialize(upload));
    public OutboxItem Enqueue(SupplementRequest feedback) => Add("feedback", Wire.Serialize(feedback));
    OutboxItem Add(string kind, string payload)
    {
        var item = new OutboxItem(Wire.Id(), DateTimeOffset.UtcNow, kind, payload);
        lock (gate) Save(item); return item;
    }
    void Save(OutboxItem item) => LocalFiles.SaveSecret(Path.Combine(Root, "outbox", item.Id + ".secret"), Wire.Serialize(item));
    public List<OutboxItem> Items()
    {
        lock (gate) return Directory.GetFiles(Path.Combine(Root, "outbox"), "*.secret").Select(x => Wire.Parse<OutboxItem>(LocalFiles.ReadSecret(x))).OrderBy(x => x.CreatedAt).ToList();
    }
    public void Complete(string id) { lock (gate) File.Delete(Path.Combine(Root, "outbox", id + ".secret")); }
    public void Fail(OutboxItem item, string error) { lock (gate) Save(item with { Error = error }); }
    public void Cache(MemberStatus status) => LocalFiles.SaveSecret(Path.Combine(Root, "status-" + status.Day + ".secret"), Wire.Serialize(status));
    public MemberStatus? Cached(string day) => File.Exists(Path.Combine(Root, "status-" + day + ".secret")) ? Wire.Parse<MemberStatus>(LocalFiles.ReadSecret(Path.Combine(Root, "status-" + day + ".secret"))) : null;
    public void SaveDraft(string reportId, string text) => LocalFiles.SaveSecret(Path.Combine(Root, "draft-" + reportId + ".secret"), text);
    public string Draft(string reportId) => File.Exists(Path.Combine(Root, "draft-" + reportId + ".secret")) ? LocalFiles.ReadSecret(Path.Combine(Root, "draft-" + reportId + ".secret")) : "";
    public string LatestDraft() => Directory.GetFiles(Root, "draft-*.secret").OrderByDescending(File.GetLastWriteTimeUtc).Select(LocalFiles.ReadSecret).FirstOrDefault() ?? "";
}

public sealed class MemberSync(MemberRepository repository)
{
    readonly SemaphoreSlim gate = new(1);
    public async Task<string> Flush(CancellationToken ct)
    {
        if (!await gate.WaitAsync(0, ct)) return "同步正在进行";
        try
        {
            var pairing = repository.Pairing;
            if (pairing == null) return "尚未绑定部门";
            using var client = new MemberConnection(pairing.Url, pairing.Fingerprint, pairing.Identity.Token);
            foreach (var item in repository.Items())
            {
                if (item.Error != null) continue;
                try
                {
                    if (item.Kind == "upload") await client.Submit(Wire.Parse<Upload>(item.Payload), ct);
                    else await client.Feedback(Wire.Parse<SupplementRequest>(item.Payload), ct);
                    repository.Complete(item.Id);
                }
                catch (ApiException ex) when (!ex.Retryable) { repository.Fail(item, $"HTTP {ex.Status}：{ex.Message}"); if (ex.Status == 401) break; }
                catch (Exception ex) when (ex is HttpRequestException or TaskCanceledException || ex is ApiException { Retryable: true })
                { return "暂未送达，已保存并将在恢复连接后重试"; }
            }
            var remaining = repository.Items();
            return remaining.Count == 0 ? "待发送队列已处理" : $"有 {remaining.Count} 项未送达：" + string.Join("；", remaining.Where(x => x.Error != null).Select(x => x.Error));
        }
        finally { gate.Release(); }
    }
}
