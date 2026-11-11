using System.Net;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
using System.Threading.RateLimiting;
using DailyReport.Core;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.RateLimiting;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;

namespace DailyReport.Server;

public sealed class DepartmentHost(DepartmentService service, Documents documents) : IAsyncDisposable
{
    WebApplication? app;
    X509Certificate2? certificate;
    public string Fingerprint => Certificate.GetCertHashString(HashAlgorithmName.SHA256);
    X509Certificate2 Certificate => certificate ??= LoadCertificate(service.Store.Root);
    public async Task Start(CancellationToken ct = default)
    {
        var settings = service.Settings;
        var builder = WebApplication.CreateSlimBuilder(new WebApplicationOptions { Args = [], ApplicationName = typeof(DepartmentHost).Assembly.FullName });
        builder.Logging.ClearProviders(); // Avoid logging request bodies, auth headers, or upstream secrets.
        // A localhost deployment (including acceptance tests) must not open a LAN listener.
        var bindAddress = new Uri(settings.AdvertisedUrl).IsLoopback ? IPAddress.Loopback : IPAddress.Any;
        builder.WebHost.ConfigureKestrel(k => { k.Limits.MaxRequestBodySize = 64 * 1024 * 1024; k.Listen(bindAddress, settings.Port, l => l.UseHttps(Certificate)); });
        builder.Services.ConfigureHttpJsonOptions(o => { foreach (var converter in Wire.Json.Converters) o.SerializerOptions.Converters.Add(converter); });
        builder.Services.AddRateLimiter(o =>
        {
            o.RejectionStatusCode = 429;
            o.AddPolicy("pair", ctx => RateLimitPartition.GetFixedWindowLimiter(ctx.Connection.RemoteIpAddress?.ToString() ?? "unknown", _ => new FixedWindowRateLimiterOptions { PermitLimit = 15, Window = TimeSpan.FromMinutes(1), QueueLimit = 0 }));
        });
        app = builder.Build();
        app.Use(async (context, next) =>
        {
            try
            {
                if (context.Request.Path != "/v1/pair")
                {
                    string header = context.Request.Headers.Authorization.ToString();
                    if (!header.StartsWith("Bearer ", StringComparison.Ordinal)) throw new DomainException("需要设备身份", 401);
                    context.Items["member"] = service.Authenticate(header[7..]);
                }
                context.Response.Headers.CacheControl = "no-store";
                await next(context);
            }
            catch (DomainException ex) { context.Response.StatusCode = ex.Status; await context.Response.WriteAsJsonAsync(new { error = ex.Message }); }
            catch (BadHttpRequestException) { context.Response.StatusCode = 400; await context.Response.WriteAsJsonAsync(new { error = "请求格式或大小不受支持" }); }
            catch (Exception) when (!context.Response.HasStarted) { context.Response.StatusCode = 500; await context.Response.WriteAsJsonAsync(new { error = "部门端处理失败，请稍后重试或联系组长" }); }
        });
        app.UseRateLimiter();
        app.MapPost("/v1/pair", (PairRequest request) => service.Pair(request)).RequireRateLimiting("pair");
        app.MapPost("/v1/submissions", (HttpContext c, Upload request) => service.Submit(MemberId(c), request));
        app.MapGet("/v1/status/{day}", (HttpContext c, string day) => service.Status(MemberId(c), day));
        app.MapPost("/v1/supplements", (HttpContext c, SupplementRequest request) => service.Supplement(MemberId(c), request));
        app.MapGet("/v1/reports/{id}/pdf", (HttpContext c, string id) =>
        {
            var report = service.Store.Read(r => r.Get<Report>(id));
            if (report == null || report.MemberId != MemberId(c) || report.ReleasedAt == null || report.FinalizedAt == null) return Results.NotFound(new { error = "正式报告不存在或尚未定稿" });
            var path = documents.BasePath(report) + ".pdf";
            return File.Exists(path) ? Results.File(path, "application/pdf", $"{report.Day}-V{report.Version}.pdf") : Results.NotFound(new { error = "归档文件缺失，请联系组长" });
        });
        await app.StartAsync(ct);
    }
    static string MemberId(HttpContext c) => (string)c.Items["member"]!;
    public static X509Certificate2 LoadCertificate(string root)
    {
        string path = Path.Combine(root, "certificate.secret");
        if (File.Exists(path)) return X509CertificateLoader.LoadPkcs12(Convert.FromBase64String(LocalFiles.ReadSecret(path)), null, X509KeyStorageFlags.UserKeySet);
        using var rsa = RSA.Create(3072);
        var request = new CertificateRequest("CN=Codex Department Daily Report", rsa, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
        request.CertificateExtensions.Add(new X509BasicConstraintsExtension(false, false, 0, true));
        request.CertificateExtensions.Add(new X509KeyUsageExtension(X509KeyUsageFlags.DigitalSignature | X509KeyUsageFlags.KeyEncipherment, true));
        request.CertificateExtensions.Add(new X509EnhancedKeyUsageExtension(new OidCollection { new("1.3.6.1.5.5.7.3.1") }, true));
        var san = new SubjectAlternativeNameBuilder(); san.AddDnsName(Environment.MachineName); san.AddDnsName("localhost"); san.AddIpAddress(IPAddress.Loopback); request.CertificateExtensions.Add(san.Build());
        using var cert = request.CreateSelfSigned(DateTimeOffset.UtcNow.AddDays(-1), DateTimeOffset.UtcNow.AddYears(5));
        var bytes = cert.Export(X509ContentType.Pfx); LocalFiles.SaveSecret(path, Convert.ToBase64String(bytes));
        return X509CertificateLoader.LoadPkcs12(bytes, null, X509KeyStorageFlags.UserKeySet);
    }
    public async ValueTask DisposeAsync() { if (app != null) { await app.StopAsync(); await app.DisposeAsync(); app = null; } certificate?.Dispose(); certificate = null; }
}
