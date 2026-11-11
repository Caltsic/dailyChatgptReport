using DailyReport.Core;

namespace DailyReport.Server;

public sealed class GenerationWorker(DepartmentService service, IReportAgent agent) : IAsyncDisposable
{
    readonly CancellationTokenSource stop = new();
    Task? loop;
    public void Start()
    {
        service.Store.Write(r => { foreach (var j in r.All<Job>().Where(j => j.State == "running")) r.Put(j.Id, j with { State = "queued", Error = null }, j.Day); });
        loop = Loop(stop.Token);
    }
    async Task Loop(CancellationToken ct)
    {
        var active = new List<Task>();
        try
        {
            while (!ct.IsCancellationRequested)
            {
                var queued = service.Store.Write(r =>
                {
                    var jobs = r.All<Job>().Where(j => j.State == "queued").ToList();
                    foreach (var j in jobs) r.Put(j.Id, j with { State = "running" }, j.Day);
                    return jobs;
                });
                foreach (var job in queued) active.Add(Run(job));
                active.RemoveAll(t => t.IsCompleted);
                await Task.Delay(1000, ct);
            }
        }
        catch (OperationCanceledException) when (ct.IsCancellationRequested) { }
        finally { await Task.WhenAll(active); }
        async Task Run(Job job) { try { await Process(job, ct); } catch (Exception) { /* Process stores the failure for this task. */ } }
    }
    public async Task Process(Job job, CancellationToken ct)
    {
        bool proceed = service.Store.Write(r =>
        {
            var current = r.Get<Job>(job.Id) ?? throw new InvalidDataException("生成任务不存在");
            if (current.State == "completed") return false;
            r.Put(current.Id, current with { State = "running", Error = null, Progress = JobTracker.Note(new(), current.Progress == null ? "开始整理报告" : "继续当前 Agent 任务") }, current.Day);
            return true;
        });
        if (!proceed) return;
        var tracker = new JobTracker(service.Store, job.Id);
        try { if (job.Kind == "department") await Department(job, tracker, ct); else await Personal(job, tracker, ct); }
        catch (OperationCanceledException) when (ct.IsCancellationRequested) { tracker.Stop("queued", "执行已暂停，启动后继续当前 Agent 任务"); throw; }
        catch (Exception ex)
        {
            tracker.Stop("failed", ex is InvalidOperationException or InvalidDataException or TimeoutException ? ex.Message : "生成未完成：" + ex.GetType().Name);
            throw;
        }
    }
    async Task Personal(Job job, JobTracker tracker, CancellationToken ct)
    {
        tracker.Stage("读取提交记录");
        var snapshot = service.Store.Read(r => r.Get<Snapshot>(job.SnapshotId)) ?? throw new InvalidDataException("来源记录不存在");
        var capture = service.LoadCapture(snapshot); var settings = service.Settings;
        var member = service.Store.Read(r => r.Get<Member>(job.MemberId))!;
        tracker.Stage("个人日报 Agent", current: $"{capture.SessionCount} 个会话 · {capture.Events.Count} 条记录"); tracker.Waiting();
        var body = await agent.Generate(job.Id, "personal", PromptVault.Read("personal.v2"), new
        {
            department = settings.Name, employee = member.Name, day = job.Day, capture.TimeZone, capture.CapturedAt,
            capture.SessionCount, collectionNotes = capture.Warnings, events = capture.Events
        }, ct, tracker.Model);
        tracker.Stage("保存并下发日报");
        service.Store.Write(r =>
        {
            if (r.Get<Job>(job.Id)?.State == "completed") return;
            var report = new Report
            {
                MemberId = member.Id, MemberName = member.Name, Department = settings.Name, Day = job.Day,
                Version = (DepartmentService.LatestReport(r, member.Id, job.Day)?.Version ?? 0) + 1,
                SnapshotId = snapshot.Id, SnapshotVersion = snapshot.Version, CapturedAt = capture.CapturedAt, SessionCount = capture.SessionCount,
                Warnings = capture.Warnings, Body = body, PromptVersion = "personal.v2", Model = settings.Model, TimeZone = capture.TimeZone,
                ReleasedAt = DateTimeOffset.UtcNow
            };
            r.Put(report.Id, report, job.Day); tracker.Complete(r); DepartmentService.Log(r, "生成并下发个人日报", report.Id);
        });
    }
    async Task Department(Job job, JobTracker tracker, CancellationToken ct)
    {
        var inputPath = Path.Combine(service.Store.Root, "agent", job.Id, "department-input.json");
        // Resume uses exactly the report versions whose input is stored in its checkpoint.
        DepartmentAgentInput input;
        if (File.Exists(inputPath)) input = Wire.Parse<DepartmentAgentInput>(File.ReadAllText(inputPath));
        else
        {
            var current = service.SummaryInput(job.Day);
            input = new(service.Settings.Name, job.Day, current.Reports, current.Missing, current.Signature);
            LocalFiles.AtomicText(inputPath, Wire.Serialize(input));
        }
        if (input.Reports.Count == 0) throw new InvalidOperationException("尚无已提交补充说明的员工日报");
        tracker.Stage("部门汇总 Agent", current: $"合并阅读 {input.Reports.Count} 份已复核日报"); tracker.Waiting();
        var body = await agent.Generate(job.Id, "department", PromptVault.Read("department.v2"), new
        {
            input.Department, input.Day,
            reports = input.Reports.Select(r => new { r.Id, r.MemberName, r.Version, body = r.Body ?? ReportText.Preview(r), supplement = r.Feedback }),
            missingMembers = input.Missing
        }, ct, tracker.Model);
        tracker.Stage("保存部门汇总");
        service.Store.Write(r =>
        {
            if (r.Get<Job>(job.Id)?.State == "completed") return;
            var report = new DepartmentReport
            {
                Day = job.Day, Version = r.All<DepartmentReport>(job.Day).Count + 1, Department = input.Department, Body = body,
                MissingMembers = input.Missing, SourceSignature = input.Signature,
                Sources = input.Reports.ToDictionary(r => r.Id, r => $"{r.MemberName}　{r.Day}　V{r.Version}"),
                PromptVersion = "department.v2", Model = service.Settings.Model
            };
            r.Put(report.Id, report, job.Day); tracker.Complete(r); DepartmentService.Log(r, "生成部门汇总", report.Id);
        });
    }
    public async ValueTask DisposeAsync() { await stop.CancelAsync(); if (loop != null) await loop; stop.Dispose(); }
}

internal record DepartmentAgentInput(string Department, string Day, List<Report> Reports, List<string> Missing, string Signature);
