using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace DailyReport.Core;

public static class Wire
{
    public const int Protocol = 1;
    public static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web)
    {
        WriteIndented = false, Converters = { new JsonStringEnumConverter() }
    };
    public static string Serialize<T>(T value) => JsonSerializer.Serialize(value, Json);
    public static T Parse<T>(string json) => JsonSerializer.Deserialize<T>(json, Json) ?? throw new InvalidDataException("数据为空");
    public static string Hash(string text) => Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(text)));
    public static string Id() => Guid.NewGuid().ToString("N");
    public static DateOnly Today(string zone) => DateOnly.FromDateTime(TimeZoneInfo.ConvertTime(DateTimeOffset.UtcNow, TimeZoneInfo.FindSystemTimeZoneById(zone)).DateTime);
}

public record Evidence(string Id, string SessionId, DateTimeOffset Timestamp, string Kind, string Text, string Source);
public record Capture(string Day, string TimeZone, DateTimeOffset CapturedAt, List<Evidence> Events, List<string> Warnings, int SessionCount, int Protocol = Wire.Protocol);
public record Upload(string RequestId, string Hash, Capture Capture);
public record Receipt(string SnapshotId, int Version, DateTimeOffset ReceivedAt);
public record Invitation(string Url, string Fingerprint, string Code, string Department, string ModelDestination, string TimeZone, int Protocol = Wire.Protocol);
public record PairRequest(string Code, string DeviceName, string PairingRequestId);
public record PairReply(string MemberId, string MemberName, string DepartmentId, string DepartmentName, string Token, string TimeZone, string ModelDestination);
public record Pairing(string Url, string Fingerprint, PairReply Identity);
public record SupplementRequest(string RequestId, string ReportId, string Text, bool NoSupplement);
public record ResubmitNotice(string Id, string Day, string Reason, DateTimeOffset CreatedAt, bool Fulfilled);
public record MemberStatus(string Day, string SubmissionStatus, string ReportStatus, Receipt? Receipt, Report? Report, List<ResubmitNotice> Notices, string DepartmentName, string ModelDestination, string TimeZone, DateTimeOffset? CapturedAt = null, int SessionCount = 0, List<string>? CaptureWarnings = null, string? AnalysisProgress = null);

public record Finding(string Text, string Status, List<string> EvidenceIds);
public record ReportContent(List<Finding> Tasks, List<Finding> Outputs, List<Finding> Verification, List<Finding> Blockers, List<Finding> NextSteps);
public record Feedback(string Text, bool NoSupplement, DateTimeOffset SubmittedAt, string Author);
public record Report
{
    public string Id { get; init; } = Wire.Id();
    public string MemberId { get; init; } = "";
    public string MemberName { get; init; } = "";
    public string Department { get; init; } = "";
    public string Day { get; init; } = "";
    public int Version { get; init; }
    public string SnapshotId { get; init; } = "";
    public int SnapshotVersion { get; init; }
    public DateTimeOffset CapturedAt { get; init; }
    public int SessionCount { get; init; }
    public List<string> Warnings { get; init; } = [];
    public ReportContent Content { get; init; } = new([], [], [], [], []);
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Body { get; init; }
    public Dictionary<string, string> Sources { get; init; } = [];
    public string PromptVersion { get; init; } = "";
    public string Model { get; init; } = "";
    public DateTimeOffset CreatedAt { get; init; } = DateTimeOffset.UtcNow;
    public DateTimeOffset? ReleasedAt { get; init; }
    public DateTimeOffset? FinalizedAt { get; init; }
    public Feedback? Feedback { get; init; }
    public string TimeZone { get; init; } = "Asia/Shanghai";
}

public record SummaryItem(string Text, List<string> ReportIds, bool PersonalStatement = false);
public record DepartmentContent(List<SummaryItem> Overview, List<SummaryItem> Members, List<SummaryItem> Blockers, List<SummaryItem> NextSteps);
public record DepartmentReport
{
    public string Id { get; init; } = Wire.Id();
    public string Day { get; init; } = "";
    public int Version { get; init; }
    public string Department { get; init; } = "";
    public DepartmentContent Content { get; init; } = new([], [], [], []);
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Body { get; init; }
    public Dictionary<string, string> Sources { get; init; } = [];
    public List<string> MissingMembers { get; init; } = [];
    public string SourceSignature { get; init; } = "";
    public string PromptVersion { get; init; } = "";
    public string Model { get; init; } = "";
    public DateTimeOffset CreatedAt { get; init; } = DateTimeOffset.UtcNow;
}

public static class ReportText
{
    public static IEnumerable<(string Heading, string Text)> Sections(Report r)
    {
        if (r.Body != null)
        {
            yield return ("基本信息与采集范围", $"部门：{r.Department}　成员：{r.MemberName}　日期：{r.Day}\n报告编号：{r.Id}　版本：V{r.Version}\n采集截至：{TimeZoneInfo.ConvertTime(r.CapturedAt, TimeZoneInfo.FindSystemTimeZoneById(r.TimeZone)):yyyy-MM-dd HH:mm:ss}（{r.TimeZone}）　会话数：{r.SessionCount}");
            yield return ("工作正文", r.Body);
            yield return ("个人补充说明", FeedbackText(r));
            yield break;
        }
        yield return ("基本信息与采集范围", $"部门：{r.Department}　成员：{r.MemberName}　日期：{r.Day}\n报告编号：{r.Id}　版本：V{r.Version}\n采集截至：{r.CapturedAt:O}　时区：{r.TimeZone}　会话数：{r.SessionCount}\n来源版本：V{r.SnapshotVersion}　提示词：{r.PromptVersion}　模型：{r.Model}\n状态：{(r.FinalizedAt is null ? "待定稿" : "已定稿 " + r.FinalizedAt.Value.ToString("O"))}\n{string.Join("\n", r.Warnings)}");
        yield return ("一、当日任务", Lines(r.Content.Tasks));
        yield return ("二、实际执行与产出", Lines(r.Content.Outputs));
        yield return ("三、验证情况", Lines(r.Content.Verification));
        yield return ("四、未完成事项与阻塞", Lines(r.Content.Blockers));
        yield return ("五、后续计划", Lines(r.Content.NextSteps));
        yield return ("来源索引", string.Join("\n", r.Sources.Select(p => $"[{p.Key}] {p.Value}")));
        yield return ("六、个人补充说明", FeedbackText(r));
    }
    public static IEnumerable<(string Heading, string Text)> Sections(DepartmentReport r)
    {
        if (r.Body != null)
        {
            yield return ("基本信息", $"部门：{r.Department}　日期：{r.Day}　版本：V{r.Version}\n报告编号：{r.Id}");
            yield return ("工作正文", r.Body);
            yield return ("参与汇总的日报版本", string.Join("\n", r.Sources.Values));
            if (r.MissingMembers.Count > 0) yield return ("未纳入汇总的成员", string.Join("\n", r.MissingMembers));
            yield break;
        }
        yield return ("基本信息", $"部门：{r.Department}　日期：{r.Day}　版本：V{r.Version}\n报告编号：{r.Id}\n生成时间：{r.CreatedAt:O}　提示词：{r.PromptVersion}　模型：{r.Model}");
        yield return ("一、总体进展", SummaryLines(r.Content.Overview));
        yield return ("二、成员工作摘要", SummaryLines(r.Content.Members));
        yield return ("三、共同阻塞", SummaryLines(r.Content.Blockers));
        yield return ("四、后续安排", SummaryLines(r.Content.NextSteps));
        yield return ("五、缺失清单", r.MissingMembers.Count == 0 ? "无" : string.Join("\n", r.MissingMembers));
        yield return ("来源日报版本", string.Join("\n", r.Sources.Select(p => $"[{p.Key}] {p.Value}")));
    }
    static string Lines(IEnumerable<Finding> list) => string.Join("\n\n", list.Select(x => $"【{x.Status}】{x.Text}\n来源：{string.Join("、", x.EvidenceIds)}")) is { Length: > 0 } s ? s : "记录中无可确认内容";
    static string FeedbackText(Report r) => r.Feedback is not { } f ? "等待成员补充说明" : $"作者：{f.Author}　提交时间：{TimeZoneInfo.ConvertTime(f.SubmittedAt, TimeZoneInfo.FindSystemTimeZoneById(r.TimeZone)):yyyy-MM-dd HH:mm:ss}　正文版本：V{r.Version}\n" + (f.NoSupplement ? "无补充说明" : f.Text);
    static string SummaryLines(IEnumerable<SummaryItem> list) => string.Join("\n\n", list.Select(x => $"{(x.PersonalStatement ? "【成员陈述，未经独立核验】" : "")}{x.Text}\n来源日报：{string.Join("、", x.ReportIds)}")) is { Length: > 0 } s ? s : "无可确认内容";
    public static string Preview(Report r) => string.Join("\n\n", Sections(r).Select(s => s.Heading + "\n" + s.Text));
    public static string Preview(DepartmentReport r) => string.Join("\n\n", Sections(r).Select(s => s.Heading + "\n" + s.Text));
}
