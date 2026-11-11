using System.IO.Compression;
using System.Security.Cryptography;
using System.Text;
using DailyReport.Core;

namespace DailyReport.Server;

public sealed class DepartmentService(Store store)
{
    public Store Store => store;
    public DepartmentSettings Settings => store.Read(r => r.Get<DepartmentSettings>("settings")) ?? throw new DomainException("请先初始化部门", 503);
    public void Configure(DepartmentSettings settings, string? apiKey = null)
    {
        if (string.IsNullOrWhiteSpace(settings.Name) || settings.Name.Length > 100 || settings.Port is < 1024 or > 65535) throw new DomainException("部门设置无效", 400);
        _ = TimeZoneInfo.FindSystemTimeZoneById(settings.TimeZone);
        if (!Uri.TryCreate(settings.AdvertisedUrl, UriKind.Absolute, out var url) || url.Scheme != "https" || !string.IsNullOrEmpty(url.UserInfo) || url.AbsolutePath != "/") throw new DomainException("部门地址必须为 HTTPS 根地址", 400);
        if (settings.ApiBaseUrl.Length > 0 && (!Uri.TryCreate(settings.ApiBaseUrl, UriKind.Absolute, out var api) || api.Scheme != "https" && !(api.Scheme == "http" && api.IsLoopback) || !string.IsNullOrEmpty(api.UserInfo) || !string.IsNullOrEmpty(api.Query))) throw new DomainException("模型地址需使用 HTTPS（本机测试可使用 HTTP）", 400);
        store.Write(r =>
        {
            var old = r.Get<DepartmentSettings>("settings");
            if (old != null && old.Id != settings.Id) throw new DomainException("不能更换部门身份");
            if (old != null && old.TimeZone != settings.TimeZone && r.All<Snapshot>().Count > 0) throw new DomainException("已有记录后不能修改部门时区");
            if (apiKey is { Length: > 0 }) LocalFiles.SaveSecret(Path.Combine(store.Root, "api-key.secret"), apiKey);
            r.Put("settings", settings); Log(r, "配置部门", settings.Name);
        });
    }
    public Member AddMember(string name)
    {
        name = name.Trim(); if (name.Length is < 1 or > 60) throw new DomainException("姓名长度需为 1–60 字", 400);
        return store.Write(r => { if (r.All<Member>().Any(m => m.Name == name && m.Active)) throw new DomainException("成员姓名已存在"); var m = new Member(Wire.Id(), name); r.Put(m.Id, m); Log(r, "新增成员", m.Id); return m; });
    }
    public List<Member> Members() => store.Read(r => r.All<Member>().Where(m => m.Active).ToList());
    public Invitation Invite(string memberId, string fingerprint)
    {
        var code = Convert.ToHexString(RandomNumberGenerator.GetBytes(24));
        store.Write(r => { RequireMember(r, memberId); var inv = new InviteRecord(Wire.Hash(code), memberId, DateTimeOffset.UtcNow.AddMinutes(30)); r.Put(inv.Id, inv); Log(r, "创建配对邀请", memberId); });
        var s = Settings; return new(s.AdvertisedUrl, fingerprint, code, s.Name, s.ApiBaseUrl, s.TimeZone);
    }
    public PairReply Pair(PairRequest request)
    {
        if (request == null || string.IsNullOrWhiteSpace(request.Code) || !Guid.TryParseExact(request.PairingRequestId, "N", out _) || string.IsNullOrWhiteSpace(request.DeviceName) || request.DeviceName.Length > 100) throw new DomainException("配对请求无效", 400);
        return store.Write(r =>
        {
            var invite = r.Get<InviteRecord>(Wire.Hash(request.Code));
            if (invite == null || invite.ExpiresAt < DateTimeOffset.UtcNow) throw new DomainException("配对码无效或已过期", 401);
            var member = RequireMember(r, invite.MemberId); string token;
            if (invite.PairingRequestId != null)
            {
                if (invite.PairingRequestId != request.PairingRequestId) throw new DomainException("配对码已使用", 401);
                if (r.Get<Device>(invite.DeviceId!) is not { Revoked: false }) throw new DomainException("设备已撤销", 401);
                token = Unprotect(invite.ProtectedToken!);
            }
            else
            {
                // A member has one current device. Re-pairing revokes the previous device.
                foreach (var old in r.All<Device>().Where(d => d.MemberId == member.Id)) r.Put(old.Id, old with { Revoked = true });
                token = Convert.ToHexString(RandomNumberGenerator.GetBytes(32));
                var device = new Device(Wire.Id(), member.Id, request.DeviceName, Wire.Hash(token));
                r.Put(device.Id, device); r.Put(invite.Id, invite with { PairingRequestId = request.PairingRequestId, DeviceId = device.Id, ProtectedToken = Protect(token) });
                Log(r, "成员设备配对", member.Id);
            }
            var s = r.Get<DepartmentSettings>("settings")!;
            return new PairReply(member.Id, member.Name, s.Id, s.Name, token, s.TimeZone, s.ApiBaseUrl);
        });
    }
    public string Authenticate(string token) => store.Read(r =>
    {
        if (token.Length != 64) throw new DomainException("设备身份无效，请重新配对", 401);
        var hash = Wire.Hash(token); var device = r.All<Device>().FirstOrDefault(d => !d.Revoked && d.TokenHash == hash);
        if (device == null || r.Get<Member>(device.MemberId) is not { Active: true }) throw new DomainException("设备身份无效或已撤销，请重新配对", 401);
        return device.MemberId;
    });
    public void Revoke(string memberId) => store.Write(r => { foreach (var d in r.All<Device>().Where(d => d.MemberId == memberId)) r.Put(d.Id, d with { Revoked = true }); Log(r, "撤销设备", memberId); });
    public Receipt Submit(string memberId, Upload upload)
    {
        if (upload?.Capture == null || !Guid.TryParseExact(upload.RequestId, "N", out _)) throw new DomainException("提交格式无效", 400);
        string json = Wire.Serialize(upload.Capture);
        if (Wire.Hash(json) != upload.Hash) throw new DomainException("内容校验失败", 400);
        ValidateCapture(upload.Capture);
        return store.Write(r =>
        {
            RequireMember(r, memberId);
            string key = memberId + ":" + upload.RequestId;
            var operation = r.Get<Operation>(key);
            if (operation != null)
            {
                if (operation.Hash != upload.Hash || operation.Kind != "upload") throw new DomainException("请求标识已用于其他内容");
                return ReceiptOf(r.Get<Snapshot>(operation.ResultId)!);
            }
            var cap = upload.Capture; EnsureOpen(r, memberId, cap.Day);
            var old = LatestSnapshot(r, memberId, cap.Day);
            if (old != null && cap.CapturedAt < old.CapturedAt) throw new DomainException("已有更新的采集记录，请重新采集后提交");
            var id = Wire.Id(); string file = id + ".json.gz";
            var temp = Path.Combine(store.Root, "submissions", file + ".tmp");
            using (var output = File.Create(temp)) using (var gzip = new GZipStream(output, CompressionLevel.Optimal)) { var bytes = Encoding.UTF8.GetBytes(json); gzip.Write(bytes); }
            File.Move(temp, Path.Combine(store.Root, "submissions", file));
            var snapshot = new Snapshot(id, memberId, cap.Day, (old?.Version ?? 0) + 1, upload.RequestId, upload.Hash, file, DateTimeOffset.UtcNow, cap.CapturedAt, cap.SessionCount, cap.Warnings);
            r.Put(id, snapshot, cap.Day); r.Put(key, new Operation(key, memberId, upload.Hash, id, "upload"), cap.Day);
            foreach (var notice in r.All<Notice>(cap.Day).Where(n => n.MemberId == memberId && !n.Fulfilled && cap.CapturedAt >= n.CreatedAt)) r.Put(notice.Id, notice with { Fulfilled = true }, cap.Day);
            Log(r, "接收记录", id); return ReceiptOf(snapshot);
        });
    }
    void ValidateCapture(Capture c)
    {
        var settings = Settings;
        if (c.Protocol != Wire.Protocol || c.TimeZone != settings.TimeZone || !DateOnly.TryParseExact(c.Day, "yyyy-MM-dd", out var day) || day > Wire.Today(settings.TimeZone)) throw new DomainException("协议、日期或部门时区不匹配", 400);
        if (c.Events == null || c.Warnings == null || c.Events.Count > 100000 || c.Warnings.Count > 10000 || c.CapturedAt > DateTimeOffset.UtcNow.AddMinutes(5)) throw new DomainException("采集范围无效或过大", 400);
        var tz = TimeZoneInfo.FindSystemTimeZoneById(settings.TimeZone);
        var ids = new HashSet<string>();
        foreach (var e in c.Events)
            if (e == null || string.IsNullOrWhiteSpace(e.Id) || !ids.Add(e.Id) || string.IsNullOrWhiteSpace(e.SessionId) || e.Text == null || e.Source == null || e.Kind is not ("user" or "assistant" or "tool_call" or "tool_result") || e.Timestamp > c.CapturedAt || DateOnly.FromDateTime(TimeZoneInfo.ConvertTime(e.Timestamp, tz).DateTime) != day) throw new DomainException("记录包含无效事件", 400);
        if (c.SessionCount != c.Events.Select(e => e.SessionId).Distinct().Count() || c.Warnings.Any(w => w == null || w.Length > 5000)) throw new DomainException("记录计数或警告格式错误", 400);
    }
    public Capture LoadCapture(Snapshot snapshot)
    {
        if (!Guid.TryParseExact(snapshot.Id, "N", out _) || snapshot.FileName != snapshot.Id + ".json.gz") throw new InvalidDataException("记录存储路径无效");
        using var input = File.OpenRead(Path.Combine(store.Root, "submissions", snapshot.FileName)); using var gzip = new GZipStream(input, CompressionMode.Decompress); using var reader = new StreamReader(gzip);
        var text = reader.ReadToEnd(); if (Wire.Hash(text) != snapshot.Hash) throw new InvalidDataException("存储的记录校验失败"); return Wire.Parse<Capture>(text);
    }
    public void RequestResubmit(string memberId, string day, string reason) => store.Write(r =>
    {
        RequireDay(day); RequireMember(r, memberId); EnsureOpen(r, memberId, day);
        if (string.IsNullOrWhiteSpace(reason) || reason.Length > 1000) throw new DomainException("请填写 1–1000 字重交原因", 400);
        var n = new Notice(Wire.Id(), memberId, day, reason.Trim(), DateTimeOffset.UtcNow); r.Put(n.Id, n, day); Log(r, "要求重交", memberId + ":" + day);
    });
    public Job QueueReport(string memberId, string day) => store.Write(r =>
    {
        RequireDay(day); RequireMember(r, memberId); EnsureOpen(r, memberId, day);
        var snapshot = LatestSnapshot(r, memberId, day) ?? throw new DomainException("尚无提交记录");
        if (r.All<Notice>(day).Any(n => n.MemberId == memberId && !n.Fulfilled)) throw new DomainException("成员尚未按要求重新提交");
        var pending = r.All<Job>(day).FirstOrDefault(j => j.MemberId == memberId && j.Kind == "personal" && j.State is "queued" or "running");
        if (pending != null) return pending;
        var job = new Job(Wire.Id(), "personal", day, memberId, snapshot.Id, "queued", DateTimeOffset.UtcNow); r.Put(job.Id, job, day); return job;
    });
    public void Release(string reportId) => store.Write(r =>
    {
        var report = r.Get<Report>(reportId) ?? throw new DomainException("报告不存在", 404); ValidateCurrent(r, report);
        if (report.ReleasedAt == null) r.Put(report.Id, report with { ReleasedAt = DateTimeOffset.UtcNow }, report.Day);
        Log(r, "下发报告", report.Id);
    });
    public MemberStatus Supplement(string memberId, SupplementRequest request)
    {
        if (request == null || !Guid.TryParseExact(request.RequestId, "N", out _) || request.Text == null || (!request.NoSupplement && string.IsNullOrWhiteSpace(request.Text))) throw new DomainException("请填写补充；没有补充请填写“无补充说明”", 400);
        store.Write(r =>
        {
            string key = memberId + ":" + request.RequestId; string hash = Wire.Hash(Wire.Serialize(request));
            if (r.Get<Operation>(key) is { } old) { if (old.Kind != "feedback" || old.Hash != hash) throw new DomainException("请求标识已用于其他内容"); return; }
            var report = r.Get<Report>(request.ReportId);
            if (report == null || report.MemberId != memberId || report.ReleasedAt == null) throw new DomainException("报告不存在", 404);
            EnsureOpen(r, memberId, report.Day); ValidateCurrent(r, report);
            if (report.FinalizedAt != null) throw new DomainException("此版本已定稿，请等待组长重新生成");
            var member = RequireMember(r, memberId);
            // Older member versions submit an empty text with NoSupplement=true.
            string text = request.NoSupplement ? "无补充说明" : request.Text;
            r.Put(report.Id, report with { Feedback = new Feedback(text, request.NoSupplement, DateTimeOffset.UtcNow, member.Name) }, report.Day);
            r.Put(key, new Operation(key, memberId, hash, report.Id, "feedback"), report.Day); Log(r, "收到个人补充", report.Id);
        });
        return Status(memberId, store.Read(r => r.Get<Report>(request.ReportId)!.Day));
    }
    public Report PrepareFinalization(string reportId) => store.Read(r =>
    {
        var report = r.Get<Report>(reportId) ?? throw new DomainException("报告不存在", 404);
        if (report.FinalizedAt != null)
        {
            ValidateCurrent(r, report);
            if (r.Get<DayState>(report.MemberId + ":" + report.Day)?.Closed != true) throw new DomainException("日报已重新开启，请先生成新版再定稿");
            return report;
        }
        ValidateCurrent(r, report);
        if (report.ReleasedAt == null) throw new DomainException("报告须先下发成员");
        if (!HasFeedback(report)) throw new DomainException("请等待成员复核并提交补充说明后导出；没有补充请填写“无补充说明”");
        if (r.All<Job>(report.Day).Any(j => j.MemberId == report.MemberId && j.Kind == "personal" && j.State is "queued" or "running")) throw new DomainException("报告正在重新生成，请稍后定稿");
        return report with { FinalizedAt = DateTimeOffset.UtcNow };
    });
    public void CommitFinalization(Report finalized) => store.Write(r =>
    {
        var current = r.Get<Report>(finalized.Id) ?? throw new DomainException("报告不存在", 404);
        if (current.FinalizedAt != null)
        {
            ValidateCurrent(r, current);
            if (r.Get<DayState>(current.MemberId + ":" + current.Day)?.Closed != true) throw new DomainException("日报已重新开启，请先生成新版再定稿");
            return;
        }
        ValidateCurrent(r, current);
        if (!HasFeedback(current)) throw new DomainException("请等待成员复核并提交补充说明后导出");
        if (Wire.Serialize(current with { FinalizedAt = finalized.FinalizedAt }) != Wire.Serialize(finalized)) throw new DomainException("报告或补充已更新，请重新定稿");
        if (r.All<Job>(current.Day).Any(j => j.Kind == "personal" && j.MemberId == current.MemberId && j.State is "queued" or "running")) throw new DomainException("报告正在重新生成，请稍后定稿");
        r.Put(finalized.Id, finalized, finalized.Day); string key = finalized.MemberId + ":" + finalized.Day;
        r.Put(key, new DayState(key, finalized.MemberId, finalized.Day, true), finalized.Day); Log(r, "定稿", finalized.Id);
    });
    public void Reopen(string memberId, string day) => store.Write(r =>
    {
        RequireMember(r, memberId); RequireDay(day); string key = memberId + ":" + day;
        r.Put(key, new DayState(key, memberId, day, false), day); Log(r, "重新开启日报", key);
    });
    public List<Report> Reports(string memberId, string day) => store.Read(r => r.All<Report>(day).Where(x => x.MemberId == memberId).OrderByDescending(x => x.Version).ToList());
    public MemberStatus Status(string memberId, string day) => store.Read(r =>
    {
        RequireDay(day); RequireMember(r, memberId); var row = Row(r, memberId, day);
        var snap = LatestSnapshot(r, memberId, day); var report = r.All<Report>(day).Where(x => x.MemberId == memberId && x.ReleasedAt != null).MaxBy(x => x.Version);
        var s = r.Get<DepartmentSettings>("settings")!;
        bool waitingForUpdate = report != null && (report.SnapshotId != snap?.Id || report.Id != LatestReport(r, memberId, day)?.Id);
        string reportStatus = report == null ? "等待组长下发报告" : waitingForUpdate ? "记录或正文已更新，等待组长下发新报告" : report.FinalizedAt != null ? "报告已归档" : HasFeedback(report) ? "补充已提交" : "请复核报告并填写补充说明";
        return new MemberStatus(day, row.SubmissionStatus, reportStatus, snap == null ? null : ReceiptOf(snap), report, r.All<Notice>().Where(n => n.MemberId == memberId && !n.Fulfilled).Select(n => new ResubmitNotice(n.Id, n.Day, n.Reason, n.CreatedAt, n.Fulfilled)).ToList(), s.Name, s.ApiBaseUrl, s.TimeZone, snap?.CapturedAt, snap?.SessionCount ?? 0, snap?.Warnings);
    });
    public List<DashboardRow> Dashboard(string day) => store.Read(r => { RequireDay(day); return r.All<Member>().Where(m => m.Active).Select(m => Row(r, m.Id, day)).ToList(); });
    static DashboardRow Row(Store.Rows r, string memberId, string day)
    {
        var member = RequireMember(r, memberId); var snapshot = LatestSnapshot(r, memberId, day); var report = LatestReport(r, memberId, day);
        bool notice = r.All<Notice>(day).Any(n => n.MemberId == memberId && !n.Fulfilled);
        var job = r.All<Job>(day).Where(j => j.Kind == "personal" && j.MemberId == memberId)
            .OrderByDescending(j => j.State is "queued" or "running").ThenByDescending(j => j.Progress?.UpdatedAt ?? j.CreatedAt).FirstOrDefault();
        string submission = notice ? "🟡 待重交" : snapshot == null ? "🔴 未收到" : snapshot.Warnings.Any(IsProblem) ? "🟡 已收到／采集异常" : "🟢 已收到";
        string state = job?.State is "queued" or "running" ? "生成中" : job?.State == "failed" && (report == null || (job.Progress?.UpdatedAt ?? job.CreatedAt) > report.CreatedAt) ? "生成失败：" + job.Error : report == null ? "待生成" : report.SnapshotId != snapshot?.Id ? "来源已更新" : report.FinalizedAt != null ? (r.Get<DayState>(memberId + ":" + day)?.Closed == true ? "已定稿" : "已重新开启／待生成新版") : report.ReleasedAt == null ? "待下发" : report.Feedback == null ? "待反馈" : "已反馈";
        return new(memberId, member.Name, submission, state, snapshot?.CapturedAt.ToString("O") ?? "—", snapshot?.Version ?? 0, report?.Version ?? 0, report?.Feedback == null ? "未反馈" : report.Feedback.NoSupplement ? "无补充" : "已补充", report?.Id, JobProgressText.Brief(job), job == null ? 0 : JobProgressText.Percent(job), job?.Id);
    }
    static bool IsProblem(string warning) => !warning.StartsWith("仅覆盖本机") && !warning.StartsWith("未发现可用工作记录");
    public (List<Report> Reports, List<string> Missing, string Signature) SummaryInput(string day) => store.Read(r => SummaryInput(r, day));
    static (List<Report> Reports, List<string> Missing, string Signature) SummaryInput(Store.Rows r, string day)
    {
        RequireDay(day); List<Report> reports = []; List<string> missing = [];
        foreach (var m in r.All<Member>().Where(m => m.Active))
        {
            var latest = LatestReport(r, m.Id, day); var snapshot = LatestSnapshot(r, m.Id, day);
            bool requestedAgain = r.All<Notice>(day).Any(n => n.MemberId == m.Id && !n.Fulfilled);
            bool reopened = latest?.FinalizedAt != null && r.Get<DayState>(m.Id + ":" + day)?.Closed != true;
            if (latest != null && HasFeedback(latest) && latest.SnapshotId == snapshot?.Id && !requestedAgain && !reopened) reports.Add(latest);
            else missing.Add(m.Name + "：" + Row(r, m.Id, day).ReportStatus);
        }
        // Finalizing a reviewed report does not alter the summary input; editing its body or feedback does.
        var contents = reports.OrderBy(x => x.Id).Select(x => x with { FinalizedAt = null }).ToArray();
        return (reports, missing, Wire.Hash(Wire.Serialize(new { reports = contents, missing = missing.Order().ToArray() })));
    }
    public Job QueueSummary(string day) => store.Write(r =>
    {
        RequireDay(day); if (r.All<Job>(day).FirstOrDefault(j => j.Kind == "department" && j.State is "queued" or "running") is { } old) return old;
        var job = new Job(Wire.Id(), "department", day, "", "", "queued", DateTimeOffset.UtcNow); r.Put(job.Id, job, day); return job;
    });
    public List<DepartmentReport> Summaries(string day) => store.Read(r => r.All<DepartmentReport>(day).OrderByDescending(x => x.Version).ToList());
    public List<JobView> Jobs(string day) => store.Read(r =>
    {
        RequireDay(day); var zone = r.Get<DepartmentSettings>("settings")!.TimeZone;
        return r.All<Job>(day).OrderByDescending(j => j.State is "queued" or "running").ThenByDescending(j => j.Progress?.UpdatedAt ?? j.CreatedAt).Select(j => JobProgressText.View(j, j.Kind == "department" ? "本部门" : r.Get<Member>(j.MemberId)?.Name ?? "未知成员", zone)).ToList();
    });
    public Job RetryJob(string id)
    {
        var job = store.Read(r => r.Get<Job>(id)) ?? throw new DomainException("任务不存在", 404);
        if (job.State != "failed") throw new DomainException("只能重试失败的任务");
        if (job.Kind == "personal")
        {
            var latest = store.Read(r => LatestSnapshot(r, job.MemberId, job.Day));
            if (latest?.Id != job.SnapshotId) return QueueReport(job.MemberId, job.Day);
            store.Read(r => { RequireMember(r, job.MemberId); EnsureOpen(r, job.MemberId, job.Day); if (r.All<Notice>(job.Day).Any(n => n.MemberId == job.MemberId && !n.Fulfilled)) throw new DomainException("成员尚未按要求重新提交"); return true; });
        }
        else
        {
            string path = Path.Combine(store.Root, "agent", job.Id, "department-input.json");
            if (File.Exists(path) && Wire.Parse<DepartmentAgentInput>(File.ReadAllText(path)).Signature != SummaryInput(job.Day).Signature) return QueueSummary(job.Day);
        }
        return store.Write(r =>
        {
            var pending = r.All<Job>(job.Day).FirstOrDefault(j => j.Kind == job.Kind && j.MemberId == job.MemberId && j.State is "queued" or "running");
            if (pending != null) return pending;
            var resumed = job with { State = "queued", Error = null }; r.Put(job.Id, resumed, job.Day); return resumed;
        });
    }
    public bool IsSummaryStale(DepartmentReport report) => SummaryInput(report.Day).Signature != report.SourceSignature;
    static bool HasFeedback(Report report) => report.Feedback != null && (report.Feedback.NoSupplement || !string.IsNullOrWhiteSpace(report.Feedback.Text));
    public static Snapshot? LatestSnapshot(Store.Rows r, string memberId, string day) => r.All<Snapshot>(day).Where(s => s.MemberId == memberId).MaxBy(s => s.Version);
    public static Report? LatestReport(Store.Rows r, string memberId, string day) => r.All<Report>(day).Where(s => s.MemberId == memberId).MaxBy(s => s.Version);
    static void ValidateCurrent(Store.Rows r, Report report)
    {
        if (LatestReport(r, report.MemberId, report.Day)?.Id != report.Id || LatestSnapshot(r, report.MemberId, report.Day)?.Id != report.SnapshotId) throw new DomainException("正文或来源已更新，请使用最新版本");
        if (r.All<Notice>(report.Day).Any(n => n.MemberId == report.MemberId && !n.Fulfilled)) throw new DomainException("仍有未完成的重交要求");
    }
    static void EnsureOpen(Store.Rows r, string member, string day) { if (r.Get<DayState>(member + ":" + day)?.Closed == true) throw new DomainException("该日日报已定稿，请组长重新开启后再提交"); }
    static Member RequireMember(Store.Rows r, string id) => r.Get<Member>(id) is { Active: true } m ? m : throw new DomainException("成员不存在", 404);
    static void RequireDay(string day) { if (!DateOnly.TryParseExact(day, "yyyy-MM-dd", out _)) throw new DomainException("日期格式错误", 400); }
    static Receipt ReceiptOf(Snapshot s) => new(s.Id, s.Version, s.ReceivedAt);
    internal static void Log(Store.Rows r, string action, string subject) { var e = new Audit(Wire.Id(), DateTimeOffset.UtcNow, action, subject); r.Put(e.Id, e, DateTime.UtcNow.ToString("yyyy-MM-dd")); }
    static string Protect(string text) => Convert.ToBase64String(ProtectedData.Protect(Encoding.UTF8.GetBytes(text), null, DataProtectionScope.CurrentUser));
    static string Unprotect(string text) => Encoding.UTF8.GetString(ProtectedData.Unprotect(Convert.FromBase64String(text), null, DataProtectionScope.CurrentUser));
}
