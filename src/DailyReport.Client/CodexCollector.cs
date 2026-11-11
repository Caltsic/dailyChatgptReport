using System.Text.Json;
using System.Text.RegularExpressions;
using DailyReport.Core;

namespace DailyReport.Client;

public static partial class Redactor
{
    [GeneratedRegex(@"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)")]
    private static partial Regex PrivateKey();
    [GeneratedRegex(@"(?i)\b(?:sk-[a-z0-9_-]{12,}|gh[pousr]_[a-z0-9_]{15,}|github_pat_[a-z0-9_]{15,}|AKIA[A-Z0-9]{16})")]
    private static partial Regex KnownToken();
    [GeneratedRegex("(?i)([\\\"']?(?:api[_-]?key|access[_-]?token|refresh[_-]?token|password|passwd|client[_-]?secret|authorization)[\\\"']?\\s*[:=]\\s*)(?:[\\\"'][^\\\"'\\r\\n]*[\\\"']|[^\\s,;}]+)")]
    private static partial Regex Assignment();
    [GeneratedRegex(@"(?i)\b(?:Bearer|Basic)\s+[a-z0-9._~+/=-]+")]
    private static partial Regex Bearer();
    [GeneratedRegex(@"(?i)(https?://)[^/\s:@]+:[^/\s@]+@")]
    private static partial Regex UrlPassword();
    public static string Clean(string text)
    {
        // Remove header credential values before masking the header assignment itself.
        // Otherwise "Authorization: Bearer TOKEN" would mask only "Bearer" and leave TOKEN.
        string cleaned = KnownToken().Replace(PrivateKey().Replace(text, "[私钥已脱敏]"), "[令牌已脱敏]");
        cleaned = Bearer().Replace(cleaned, "[认证内容已脱敏]");
        return UrlPassword().Replace(Assignment().Replace(cleaned, "$1[已脱敏]"), "$1[已脱敏]@");
    }
}

public sealed class CodexCollector
{
    public static string DefaultRoot => Environment.GetEnvironmentVariable("CODEX_HOME") ?? Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".codex");
    public DailyReport.Core.Capture Collect(string root, DateOnly day, string zone, CancellationToken ct = default)
    {
        var tz = TimeZoneInfo.FindSystemTimeZoneById(zone);
        var now = DateTimeOffset.UtcNow;
        if (day > Wire.Today(zone)) throw new InvalidOperationException("不能提交未来日期");
        List<Evidence> events = []; List<string> warnings = [];
        HashSet<string> seen = []; bool foundRoot = false;
        foreach (var directory in new[] { "sessions", "archived_sessions" })
        {
            var folder = Path.Combine(root, directory);
            if (!Directory.Exists(folder)) continue;
            foundRoot = true;
            string[] files;
            try { files = Directory.GetFiles(folder, "*.jsonl", SearchOption.AllDirectories); }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException) { warnings.Add($"无法扫描 {directory}，采集不完整"); continue; }
            foreach (string file in files)
            {
                ct.ThrowIfCancellationRequested();
                // Filename date is creation date, never the activity date.
                string session = Path.GetFileNameWithoutExtension(file), source = Path.GetRelativePath(root, file);
                List<Evidence> primary = [], fallback = []; Dictionary<string, string> agentBodies = []; int lineNumber = 0; bool malformed = false;
                try
                {
                    using var stream = new FileStream(file, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
                    using var reader = new StreamReader(stream);
                    while (reader.ReadLine() is { } line)
                    {
                        ct.ThrowIfCancellationRequested(); lineNumber++;
                        JsonDocument doc;
                        try { doc = JsonDocument.Parse(line); }
                        catch (JsonException) { malformed = true; continue; }
                        using (doc)
                        {
                            var row = doc.RootElement;
                            if (row.ValueKind != JsonValueKind.Object) { malformed = true; continue; }
                            string type = Str(row, "type");
                            if (!row.TryGetProperty("payload", out var p) || p.ValueKind != JsonValueKind.Object) { if (type is "response_item" or "event_msg") malformed = true; continue; }
                            if (type == "session_meta") { session = Str(p, "id") is { Length: > 0 } id ? id : session; continue; }
                            if (!DateTimeOffset.TryParse(Str(row, "timestamp"), out var time)) { if (type is "response_item" or "event_msg") malformed = true; continue; }
                            if (DateOnly.FromDateTime(TimeZoneInfo.ConvertTime(time, tz).DateTime) != day || time > now) continue;
                            string kind = "", text = "", pt = Str(p, "type"); string? agentBody = null;
                            if (type == "response_item")
                            {
                                if (pt == "message")
                                {
                                    var role = Str(p, "role"); var channel = Str(p, "channel");
                                    if (role is not ("user" or "assistant") || (role == "assistant" && channel is not ("" or "final" or "commentary"))) continue;
                                    if (!p.TryGetProperty("content", out var blocks) || blocks.ValueKind != JsonValueKind.Array || blocks.EnumerateArray().Any(x => x.ValueKind != JsonValueKind.Object || string.IsNullOrEmpty(Str(x, "type")))) malformed = true;
                                    kind = role; text = TextContent(p);
                                }
                                else if (pt == "agent_message")
                                {
                                    if (!p.TryGetProperty("content", out var blocks) || blocks.ValueKind != JsonValueKind.Array) { malformed = true; continue; }
                                    agentBody = VisibleAgentBody(TextContent(p));
                                    // An envelope with only encrypted content has no visible work record.
                                    if (string.IsNullOrWhiteSpace(agentBody)) continue;
                                    kind = "assistant";
                                    text = $"协作消息 · 作者：{Str(p, "author")} · 接收方：{Str(p, "recipient")}\n{agentBody}";
                                }
                                else if (pt is "function_call" or "custom_tool_call") { kind = "tool_call"; text = Str(p, "name") + "\n" + (pt == "function_call" ? Str(p, "arguments") : Str(p, "input")); }
                                else if (pt is "function_call_output" or "custom_tool_call_output") { kind = "tool_result"; text = SafeOutput(p); }
                                else if (pt == "web_search_call")
                                {
                                    // Only project visible action fields. Never serialize passthrough metadata.
                                    if (!TryWebSearch(p, out text)) { warnings.Add($"{source}:{lineNumber} 存在不支持或损坏的联网搜索动作，未采集该项"); continue; }
                                    kind = "tool_call"; // A completed search is not evidence that the member's task was completed.
                                }
                                else if (pt == "reasoning") continue;
                                else { warnings.Add($"{source}:{lineNumber} 存在不支持的响应类型 {pt}，未采集该项"); continue; }
                            }
                            else if (type == "event_msg" && pt is "user_message" or "agent_message")
                            {
                                if (pt == "agent_message" && Str(p, "phase") is not ("" or "final_answer" or "commentary")) continue;
                                kind = pt == "user_message" ? "user" : "assistant"; text = Str(p, "message");
                            }
                            else continue;
                            if (string.IsNullOrWhiteSpace(text)) continue;
                            text = Redactor.Clean(text);
                            var evidence = new Evidence("E" + Wire.Hash(session + ":" + lineNumber + ":" + time.ToString("O"))[..20], session, time, kind, text, source + ":" + lineNumber);
                            if (agentBody != null) agentBodies[evidence.Id] = Redactor.Clean(agentBody);
                            (type == "response_item" ? primary : fallback).Add(evidence);
                        }
                    }
                    // Event messages duplicate response items. Use fallback only for missing visible messages.
                    bool DuplicatesPrimary(Evidence f) => primary.Any(p => p.Kind == f.Kind &&
                        (p.Text == f.Text || agentBodies.TryGetValue(p.Id, out var body) && body == VisibleAgentBody(f.Text)));
                    foreach (var item in primary.Concat(fallback.Where(f => !DuplicatesPrimary(f))))
                        if (seen.Add(item.Id)) events.Add(item);
                    if (malformed) warnings.Add($"{source} 含损坏、未写完或无时间戳的记录；无法保证完整，稍后可重交");
                }
                catch (Exception e) when (e is IOException or UnauthorizedAccessException) { warnings.Add($"无法读取 {source}，采集不完整"); }
            }
        }
        if (!foundRoot) warnings.Add("未找到 Codex sessions 或 archived_sessions 目录，请检查记录路径");
        if (events.Count == 0) warnings.Add("未发现可用工作记录；不代表当天没有工作");
        warnings.Add("仅覆盖本机可读取的当日事件；跨日上下文、附件及外部文件未采集。");
        return new(day.ToString("yyyy-MM-dd"), zone, now, events.OrderBy(e => e.Timestamp).ToList(), warnings.Distinct().ToList(), events.Select(e => e.SessionId).Distinct().Count());
    }
    static string Str(JsonElement p, string name) => p.ValueKind == JsonValueKind.Object && p.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString()! : "";
    static string VisibleAgentBody(string text)
    {
        text = text.Replace("\r\n", "\n").Trim();
        if (text.StartsWith("Message Type:", StringComparison.Ordinal))
        {
            int payload = text.IndexOf("\nPayload:", StringComparison.Ordinal);
            if (payload >= 0) text = text[(payload + "\nPayload:".Length)..].Trim();
        }
        return text;
    }
    static bool TryWebSearch(JsonElement p, out string text)
    {
        text = "";
        if (!p.TryGetProperty("action", out var action) || action.ValueKind != JsonValueKind.Object) return false;
        if (p.TryGetProperty("status", out var state) && state.ValueKind is not (JsonValueKind.String or JsonValueKind.Null)) return false;
        string type = Str(action, "type"), status = Str(p, "status");
        if (type is not ("search" or "open_page" or "find_in_page")) return false;
        if (status is not ("" or "in_progress" or "searching" or "completed" or "failed")) return false;
        var visible = new Dictionary<string, object> { ["type"] = type, ["status"] = status };
        string[] fields = type switch { "search" => ["query"], "open_page" => ["url"], _ => ["url", "pattern"] };
        foreach (string field in fields)
            if (action.TryGetProperty(field, out var value))
            {
                if (value.ValueKind is JsonValueKind.Null) continue;
                if (value.ValueKind != JsonValueKind.String) return false;
                visible[field] = Redactor.Clean(value.GetString()!);
            }
        if (type == "search" && action.TryGetProperty("queries", out var queries) && queries.ValueKind != JsonValueKind.Null)
        {
            if (queries.ValueKind != JsonValueKind.Array || queries.EnumerateArray().Any(q => q.ValueKind != JsonValueKind.String)) return false;
            visible["queries"] = queries.EnumerateArray().Select(q => Redactor.Clean(q.GetString()!)).ToArray();
        }
        text = "web_search\n" + Wire.Serialize(visible) + "\n仅记录联网检索动作及其状态；此项不包含网页正文，也不证明相关工作已完成或验证。";
        return true;
    }
    static string TextContent(JsonElement p) => p.TryGetProperty("content", out var c) && c.ValueKind == JsonValueKind.Array ? string.Join("\n", c.EnumerateArray().Where(x => Str(x, "type") is "input_text" or "output_text" or "text").Select(x => Str(x, "text"))) : "";
    static string SafeOutput(JsonElement p)
    {
        if (!p.TryGetProperty("output", out var o)) return "";
        if (o.ValueKind == JsonValueKind.String) return o.GetString()!;
        if (o.ValueKind == JsonValueKind.Array) return string.Join("\n", o.EnumerateArray().Where(x => Str(x, "type") is "text" or "output_text" or "input_text").Select(x => Str(x, "text")));
        return "[非文本工具结果未采集]";
    }
}
