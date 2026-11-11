using DailyReport.Core;

namespace DailyReport.Server;

public record ProgressEvent(DateTimeOffset At, string Message);
public record ModelActivity(string Kind, string Message);
public record JobProgress
{
    public string Stage { get; init; } = "准备分析";
    public int Completed { get; init; }
    public int Total { get; init; }
    public string Current { get; init; } = "";
    public string Activity { get; init; } = "准备分析";
    public DateTimeOffset StartedAt { get; init; } = DateTimeOffset.UtcNow;
    public DateTimeOffset UpdatedAt { get; init; } = DateTimeOffset.UtcNow;
    public DateTimeOffset? RequestStartedAt { get; init; }
    public int Requests { get; init; }
    public int Retries { get; init; }
    public int Repairs { get; init; }
    public List<ProgressEvent> Events { get; init; } = [];
}

// Records only fixed diagnostic messages and source identifiers, never model bodies or credentials.
internal sealed class JobTracker(Store store, string id)
{
    public void Stage(string stage, int completed = 0, int total = 0, string current = "") => Update(p => p with
    { Stage = stage, Completed = completed, Total = total, Current = current, Activity = "准备处理", RequestStartedAt = null }, stage + (total > 0 ? $" · 已完成 {completed}/{total}" : "") + (current.Length > 0 ? " · " + current : ""));
    public void Waiting() => Update(p => p with { Activity = "等待模型响应", RequestStartedAt = DateTimeOffset.UtcNow }, "资料已准备，等待报告 Agent 响应");
    public void Model(ModelActivity activity) => Update(p => p with
    {
        Activity = activity.Message,
        RequestStartedAt = activity.Kind == "request" ? DateTimeOffset.UtcNow : p.RequestStartedAt,
        Requests = p.Requests + (activity.Kind == "request" ? 1 : 0),
        Retries = p.Retries + (activity.Kind == "retry" ? 1 : 0),
        Repairs = p.Repairs + (activity.Kind == "repair" ? 1 : 0)
    }, activity.Message);
    void Update(Func<JobProgress, JobProgress> change, string message) => store.Write(r =>
    {
        var job = r.Get<Job>(id); if (job == null || job.State != "running") return;
        var p = change(job.Progress ?? new());
        r.Put(id, job with { Progress = Note(p, message) }, job.Day);
    });
    internal static JobProgress Note(JobProgress p, string message) => p with
    { UpdatedAt = DateTimeOffset.UtcNow, Events = p.Events.TakeLast(99).Append(new ProgressEvent(DateTimeOffset.UtcNow, message)).ToList() };
    public void Complete(Store.Rows r)
    {
        var job = r.Get<Job>(id)!; var p = job.Progress ?? new();
        r.Put(id, job with { State = "completed", Error = null, Progress = Note(p with { Stage = "生成完成", Completed = 1, Total = 1, Current = "", Activity = "报告已保存", RequestStartedAt = null }, "报告已保存，生成完成") }, job.Day);
    }
    public void Stop(string state, string message) => store.Write(r =>
    {
        var job = r.Get<Job>(id); if (job == null || job.State == "completed") return;
        var p = job.Progress ?? new();
        r.Put(id, job with { State = state, Error = state == "failed" ? message : null, Progress = Note(p with { Activity = message, RequestStartedAt = null }, message) }, job.Day);
    });
}

public record JobView(string Id, string Owner, string Kind, string State, string Progress, double StagePercent, string Elapsed, string Signal, string Details);
public static class JobProgressText
{
    public static string Brief(Job? job, DateTimeOffset? clock = null)
    {
        if (job == null) return "—";
        var now = clock ?? DateTimeOffset.UtcNow; var p = job.Progress;
        if (job.State == "queued") return p == null ? "等待启动 Agent" : "等待继续当前 Agent 任务";
        if (p == null) return job.State == "failed" ? "生成失败：" + job.Error : job.State == "completed" ? "生成完成" : "生成中（旧版任务无详细进度）";
        string stage = p.Stage + (p.Total > 0 ? $" · 已完成 {p.Completed}/{p.Total}" : "") + (p.Current.Length > 0 ? " · " + p.Current : "");
        if (job.State == "failed") return stage + "\n失败：" + job.Error;
        if (job.State == "completed") return $"生成完成 · 耗时 {Duration(p.UpdatedAt - p.StartedAt)}";
        string waiting = p.RequestStartedAt is { } start ? $" · 已运行 {Duration(now - start)}" : "";
        return stage + "\n" + p.Activity + waiting;
    }
    public static double Percent(Job job) => job.Progress is { Total: > 0 } p ? Math.Clamp(100d * p.Completed / p.Total, 0, 100) : 0;
    public static JobView View(Job job, string owner, string zone, DateTimeOffset? clock = null)
    {
        var now = clock ?? DateTimeOffset.UtcNow; var p = job.Progress;
        string state = job.State switch { "queued" => "排队中", "running" => "生成中", "completed" => "已完成", "failed" => "失败", _ => job.State };
        string signal = job.State == "failed" ? "失败" : "";
        var elapsed = p == null ? "—" : Duration((job.State is "completed" or "failed" or "queued" ? p.UpdatedAt : now) - p.StartedAt);
        var tz = TimeZoneInfo.FindSystemTimeZoneById(zone);
        string details = $"{owner} · {job.Day} · {(job.Kind == "personal" ? "个人日报" : "部门汇总")}\n任务编号：{job.Id}\n状态：{state}　耗时：{elapsed}\n{Brief(job, now)}\n";
        if (p != null) details += $"\n最近反馈：{TimeZoneInfo.ConvertTime(p.UpdatedAt, tz):yyyy-MM-dd HH:mm:ss}（{zone}）\n\n执行记录：\n" + string.Join("\n", p.Events.Select(e => $"{TimeZoneInfo.ConvertTime(e.At, tz):HH:mm:ss}　{e.Message}"));
        return new(job.Id, owner, job.Kind == "personal" ? "个人日报" : "部门汇总", state, Brief(job, now), Percent(job), elapsed, signal, details);
    }
    static string Duration(TimeSpan span) { int seconds = Math.Max(0, (int)span.TotalSeconds); return seconds < 60 ? $"{seconds} 秒" : $"{seconds / 60} 分 {seconds % 60} 秒"; }
}
