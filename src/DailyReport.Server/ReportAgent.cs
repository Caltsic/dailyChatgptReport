using System.Diagnostics;
using System.Text;
using System.Text.Json;
using DailyReport.Core;

namespace DailyReport.Server;

public interface IReportAgent
{
    Task<string> Generate(string jobId, string kind, string prompt, object input, CancellationToken ct, Action<ModelActivity>? progress = null);
    Task Test(CancellationToken ct);
}

// Runs the bundled LangGraph agent on the leader computer. Credentials travel over stdin only.
public sealed class ReportAgent(Func<DepartmentSettings> settings, Func<string> key, string root) : IReportAgent, IDisposable
{
    public async Task<string> Generate(string jobId, string kind, string prompt, object input, CancellationToken ct, Action<ModelActivity>? progress = null)
    {
        var config = settings(); var apiKey = key();
        if (string.IsNullOrWhiteSpace(config.ApiBaseUrl) || string.IsNullOrWhiteSpace(config.Model)) throw new InvalidOperationException("请配置模型地址和模型名称");
        if (string.IsNullOrWhiteSpace(apiKey)) throw new InvalidOperationException("尚未配置 API Key");
        var start = Runtime();
        start.UseShellExecute = false; start.CreateNoWindow = true;
        start.RedirectStandardInput = true; start.RedirectStandardOutput = true; start.RedirectStandardError = true;
        start.StandardInputEncoding = new UTF8Encoding(false); start.StandardOutputEncoding = Encoding.UTF8; start.StandardErrorEncoding = Encoding.UTF8;
        start.Environment["PYTHONIOENCODING"] = "utf-8";
        using var process = Process.Start(start) ?? throw new InvalidOperationException("无法启动报告 Agent");
        using var cancel = ct.Register(() => { try { if (!process.HasExited) process.Kill(true); } catch (InvalidOperationException) { } });
        var errors = process.StandardError.ReadToEndAsync(ct);
        string? result = null, failure = null;
        try
        {
            string workspace = Path.Combine(root, "agent", jobId);
            await process.StandardInput.WriteLineAsync(Wire.Serialize(new { jobId, kind, systemPrompt = prompt, input, api = new { baseUrl = config.ApiBaseUrl, key = apiKey, model = config.Model }, workspace }).AsMemory(), ct);
            process.StandardInput.Close();
            while (await process.StandardOutput.ReadLineAsync(ct) is { } line)
            {
                if (string.IsNullOrWhiteSpace(line)) continue;
                using var row = JsonDocument.Parse(line); var e = row.RootElement;
                string type = e.GetProperty("type").GetString() ?? "";
                if (type == "result") result = e.GetProperty("text").GetString();
                else if (type == "error") failure = e.GetProperty("message").GetString();
                else if (type == "progress") progress?.Invoke(new("agent", e.GetProperty("message").GetString() ?? "Agent 正在整理报告"));
            }
            await process.WaitForExitAsync(ct); await errors; ct.ThrowIfCancellationRequested();
            if (failure != null) throw new InvalidOperationException(failure.Replace(apiKey, "[密钥已隐藏]").Replace(prompt, "[系统提示词已隐藏]"));
            if (process.ExitCode != 0) throw new InvalidOperationException($"报告 Agent 退出（代码 {process.ExitCode}），请重试当前任务");
            return !string.IsNullOrWhiteSpace(result) ? result : throw new InvalidDataException("模型未返回报告正文，请重试当前任务");
        }
        catch (Exception) when (ct.IsCancellationRequested) { throw new OperationCanceledException(ct); }
        finally
        {
            if (!process.HasExited) process.Kill(true);
            try { await errors; } catch (OperationCanceledException) { }
        }
    }
    public async Task Test(CancellationToken ct) => _ = await Generate(Wire.Id(), "test", "请回复：连接成功。", "测试模型连接", ct);
    static ProcessStartInfo Runtime()
    {
        string packaged = Path.Combine(AppContext.BaseDirectory, "agent", "report-agent.exe");
        if (File.Exists(packaged)) return new(packaged);
        for (var folder = new DirectoryInfo(AppContext.BaseDirectory); folder != null; folder = folder.Parent)
        {
            string python = Path.Combine(folder.FullName, ".tools", "agent-venv", "Scripts", "python.exe"), script = Path.Combine(folder.FullName, "agent", "runner.py");
            if (File.Exists(python) && File.Exists(script)) { var info = new ProcessStartInfo(python); info.ArgumentList.Add(script); return info; }
        }
        throw new InvalidOperationException("缺少报告 Agent 运行时，请使用完整组长端安装包");
    }
    public void Dispose() { }
}
