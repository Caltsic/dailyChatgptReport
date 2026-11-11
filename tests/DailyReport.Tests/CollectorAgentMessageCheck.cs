using DailyReport.Client;
using DailyReport.Core;

internal static class CollectorAgentMessageCheck
{
    // Covers the actual mixed visible/encrypted response_item.agent_message format.
    public static Task Run(string root)
    {
        string logs = Path.Combine(root, "agent-message-collection"), sessions = Path.Combine(logs, "sessions");
        Directory.CreateDirectory(sessions);
        string envelope = "Message Type: FINAL_ANSWER\nTask name: /root\nSender: /root/review\nPayload:\n";
        string body = "完成文档检查。password=test-secret";
        object Row(string type, object payload) => new { timestamp = "2026-11-08T16:10:00Z", type, payload };
        object Agent(params object[] content) => Row("response_item", new { type = "agent_message", author = "/root/review", recipient = "/root", content, internal_chat_message_metadata_passthrough = "PRIVATE_METADATA" });
        object Text(string text) => new { type = "input_text", text };
        object Encrypted() => new { type = "encrypted_content", encrypted_content = "PRIVATE_CIPHERTEXT" };
        var rows = new[] {
            Agent(Text(envelope + body), Encrypted()),
            Agent(Text(envelope), Encrypted()),
            Agent(Encrypted()),
            Row("event_msg", new { type = "agent_message", message = envelope + body, phase = "final_answer" }),
            Row("response_item", new { type = "message", role = "assistant", channel = "final", content = new[] { Text("普通助手回复") } }),
            Row("event_msg", new { type = "agent_message", message = "普通助手回复", phase = "final_answer" })
        };
        File.WriteAllLines(Path.Combine(sessions, "mixed.jsonl"), rows.Select(Wire.Serialize));
        var capture = new CodexCollector().Collect(logs, new DateOnly(2026, 11, 9), "Asia/Shanghai");
        string combined = string.Join("\n", capture.Events.Select(e => e.Text));
        if (capture.Events.Count != 2 || capture.Events.Any(e => e.Kind != "assistant") ||
            !combined.Contains("完成文档检查") || !combined.Contains("作者：/root/review") || !combined.Contains("接收方：/root") ||
            combined.Contains("PRIVATE") || combined.Contains("test-secret") || combined.Contains("Message Type:") ||
            capture.Warnings.Any(w => !w.StartsWith("仅覆盖本机")))
            throw new InvalidDataException("协作消息应保留可见正文及作者、去重，并跳过加密内容和内部元数据");
        return Task.CompletedTask;
    }
}
