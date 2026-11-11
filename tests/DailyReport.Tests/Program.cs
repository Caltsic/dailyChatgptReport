using System.Net;
using System.Net.Sockets;
using DailyReport.Client;
using DailyReport.Core;
using DailyReport.Server;
using DocumentFormat.OpenXml.Packaging;
using DocumentFormat.OpenXml.Validation;

if (args.Contains("--browser-path")) { using var info = await Microsoft.Playwright.Playwright.CreateAsync(); Console.WriteLine(info.Chromium.ExecutablePath); return 0; }
if (args.Contains("--live")) return await LiveCheck.Run(args);

string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../artifacts/acceptance", "run-" + DateTime.Now.ToString("yyyyMMdd-HHmmss")));
Directory.CreateDirectory(root); List<string> failures = []; int passed = 0;
void Check(bool ok, string message) { if (!ok) throw new Exception(message); }
async Task Scenario(string name, Func<Task> action)
{
    try { await action(); passed++; Console.WriteLine("PASS " + name); }
    catch (Exception ex) { failures.Add(name + ": " + ex.Message); Console.WriteLine("FAIL " + name + ": " + ex); }
}
async Task MustFail(Func<Task> action) { try { await action(); } catch (DomainException) { return; } catch (ApiException) { return; } throw new Exception("应拒绝该操作"); }
int Port() { using var listener = new TcpListener(IPAddress.Loopback, 0); listener.Start(); return ((IPEndPoint)listener.LocalEndpoint).Port; }
var store = new Store(Path.Combine(root, "leader")); var service = new DepartmentService(store); int port = Port();
service.Configure(new(Wire.Id(), "流程验证部", "Asia/Shanghai", port, $"https://localhost:{port}", "https://example.invalid/v1", "fixture"));
var documents = new Documents(store.Root); var agent = new FixtureAgent();
await using var worker = new GenerationWorker(service, agent);
var alice = service.AddMember("张三"); var bob = service.AddMember("李四");
string day = "2026-11-08";
var capture = new Capture(day, "Asia/Shanghai", DateTimeOffset.Parse("2026-11-08T12:00:00Z"), [new("E001", "old-session", DateTimeOffset.Parse("2026-11-08T03:00:00Z"), "user", "讨论部署方案，尚未部署", "sessions/old.jsonl:1")], [], 1);
var upload = new Upload(Wire.Id(), Wire.Hash(Wire.Serialize(capture)), capture);
await using var host = new DepartmentHost(service, documents); await host.Start();
var invite = service.Invite(alice.Id, host.Fingerprint);
using var firstConnection = new MemberConnection(invite.Url, invite.Fingerprint);
var paired = await firstConnection.Pair(new(invite.Code, "fixture-pc", Wire.Id()), default);
using var member = new MemberConnection(invite.Url, invite.Fingerprint, paired.Token);
Report? report = null;

await Scenario("成员采集、上传、离线恢复及复核补充闭环", async () =>
{
    await CollectorAgentMessageCheck.Run(root);
    var repository = new MemberRepository(Path.Combine(root, "member")); repository.Bind(new(invite.Url, invite.Fingerprint, paired)); repository.Enqueue(upload);
    await host.DisposeAsync(); await new MemberSync(repository).Flush(default);
    Check(repository.Items().Count == 1, "离线提交应保留");
    await host.Start(); await new MemberSync(new MemberRepository(repository.Root)).Flush(default);
    var receipt = await member.Submit(upload, default); Check(receipt.Version == 1 && repository.Items().Count == 0, "重启/重试不能重复入库");
    await worker.Process(service.QueueReport(alice.Id, day), default);
    report = (await member.Status(day, default)).Report;
    Check(report?.Body?.Contains("未执行部署") == true, "完整自然正文应自动下发");
    Check((await member.Status(day, default)).AnalysisProgress == null, "成员不接收 Agent 状态");
    await MustFail(() => { service.PrepareFinalization(report!.Id); return Task.CompletedTask; });
    await MustFail(() => member.Feedback(new(Wire.Id(), report!.Id, " ", false), default));
    await member.Feedback(new(Wire.Id(), report!.Id, "", true), default); // Existing member versions remain compatible.
    Check(service.Reports(alice.Id, day)[0].Feedback?.Text == "无补充说明", "无补充必须保存为实际文本");
    await MustFail(() => { service.Supplement(bob.Id, new(Wire.Id(), report!.Id, "别人的补充", false)); return Task.CompletedTask; });
    Check(service.SummaryInput(day).Reports.Count == 1, "已反馈而未定稿也应参与汇总");
});

await Scenario("完整报告汇总、补充变化及归档版本关联", async () =>
{
    string supplement = "人工核对：方案尚未部署。\n补充原文 **保持原样** <不是HTML>。";
    await member.Feedback(new(Wire.Id(), report!.Id, supplement, false), default);
    await worker.Process(service.QueueSummary(day), default);
    var summary = service.Summaries(day)[0];
    using var input = System.Text.Json.JsonDocument.Parse(agent.DepartmentInput);
    var included = input.RootElement.GetProperty("reports")[0];
    Check(included.GetProperty("supplement").GetProperty("text").GetString()!.Contains("人工核对") && included.GetProperty("body").GetString()!.Contains("未执行部署"), "部门 Agent 必须一起收到完整正文和补充");
    Check(!service.IsSummaryStale(summary), "新汇总应有效");
    await member.Feedback(new(Wire.Id(), report!.Id, supplement + "\n新增补充。", false), default);
    Check(service.IsSummaryStale(summary), "补充变化必须使汇总过期");
    await worker.Process(service.QueueSummary(day), default);
    var currentSummary = service.Summaries(day)[0]; var finalized = service.PrepareFinalization(report!.Id);
    service.CommitFinalization(finalized);
    Check(!service.IsSummaryStale(currentSummary), "仅导出归档不应使汇总过期");
    await MustFail(() => member.Feedback(new(Wire.Id(), report.Id, "迟到更正", false), default));
    service.Reopen(alice.Id, day); await worker.Process(service.QueueReport(alice.Id, day), default);
    Check(service.Reports(alice.Id, day)[0].Version == 2 && service.Reports(alice.Id, day)[0].Feedback == null, "新正文需要重新复核，旧归档保留");
    Check(service.Reports(alice.Id, day).Single(r => r.Id == report.Id).Feedback!.Text.EndsWith("新增补充。"), "旧归档不得覆盖");
});

await Scenario("独立成员并行、失败保留与同任务继续", async () =>
{
    var parallelStore = new Store(Path.Combine(root, "parallel")); var parallelService = new DepartmentService(parallelStore); parallelService.Configure(service.Settings with { Id = Wire.Id(), Name = "并行验证" });
    var gated = new ParallelAgent(); await using var concurrentWorker = new GenerationWorker(parallelService, gated);
    foreach (string name in new[] { "甲", "乙", "丙" }) { var m = parallelService.AddMember(name); parallelService.Submit(m.Id, upload); parallelService.QueueReport(m.Id, day); }
    concurrentWorker.Start(); await gated.AllEntered.Task.WaitAsync(TimeSpan.FromSeconds(15));
    Check(gated.Active == 3, "独立成员应该并行进入 Agent"); gated.Release.SetResult();
    while (parallelStore.Read(r => r.All<Job>().Any(j => j.State is "queued" or "running"))) await Task.Delay(50);
    var failed = parallelStore.Read(r => r.All<Job>().Single(j => j.State == "failed"));
    Check(parallelStore.Read(r => r.All<Report>().Count) == 2, "单人失败不影响其他成员");
    var resumed = parallelService.RetryJob(failed.Id); Check(resumed.Id == failed.Id, "来源不变应继续同一个 Agent checkpoint");
});

await Scenario("同份正文的 Word/PDF 排版与补充原文", async () =>
{
    var archived = service.Reports(alice.Id, day).Single(r => r.Id == report!.Id);
    var sections = ReportText.Sections(archived).ToList(); string word = Path.Combine(root, "排版验证.docx");
    Documents.CreateWord(word, "中文日报排版验证", sections);
    using (var doc = WordprocessingDocument.Open(word, false))
    {
        Check(!new OpenXmlValidator().Validate(doc).Any(), "Word 应可正常解析");
        string text = doc.MainDocumentPart!.Document.InnerText;
        Check(text.Contains("未执行部署") && text.Contains("**保持原样** <不是HTML>"), "正文和补充必须完整");
        Check(doc.MainDocumentPart.Document.Descendants<DocumentFormat.OpenXml.Wordprocessing.Table>().Any(), "Markdown 表格应成为 Word 表格");
    }
    if (args.Contains("--pdf"))
    {
        await documents.Export(archived); var pdf = await member.Pdf(archived.Id, default);
        Check(pdf.Length > 1000 && System.Text.Encoding.ASCII.GetString(pdf[..4]) == "%PDF", "成员应能下载正式 PDF");
        File.Copy(documents.BasePath(archived) + ".pdf", Path.Combine(root, "排版验证.pdf"));
    }
    var legacy = archived with { Body = null }; Check(!Wire.Serialize(legacy).Contains("\"body\""), "旧报告序列化不能增加空字段改变归档地址");
});

LocalFiles.AtomicText(Path.Combine(root, "result.json"), Wire.Serialize(new { passed, failed = failures.Count, failures, root }));
LocalFiles.AtomicText(Path.Combine(root, "..", "latest-result.json"), Wire.Serialize(new { passed, failed = failures.Count, failures, root, at = DateTimeOffset.UtcNow }));
Console.WriteLine($"RESULT {passed} scenarios, {failures.Count} failed; {root}"); return failures.Count == 0 ? 0 : 1;

sealed class FixtureAgent : IReportAgent
{
    public string DepartmentInput { get; private set; } = "";
    public Task Test(CancellationToken ct) => Task.CompletedTask;
    public Task<string> Generate(string jobId, string kind, string prompt, object input, CancellationToken ct, Action<ModelActivity>? progress = null)
    {
        progress?.Invoke(new("agent", "报告正文已接收"));
        if (kind == "department") { DepartmentInput = Wire.Serialize(input); return Task.FromResult("## 一、总体进展\n讨论部署方案，未执行部署。\n\n## 二、成员工作摘要\n张三完成方案讨论。"); }
        return Task.FromResult("## 一、当日任务\n讨论部署方案。\n\n## 二、实际执行与产出\n未执行部署。\n\n| 项目 | 结果 |\n| --- | --- |\n| 方案 | 已讨论 |\n| 部署 | 未执行 |\n\n## 三、验证情况\n没有部署验证记录。\n\n## 四、未完成事项与阻塞\n部署尚待执行。\n\n## 五、后续计划\n待确认部署窗口。");
    }
}
sealed class ParallelAgent : IReportAgent
{
    public readonly TaskCompletionSource AllEntered = new(TaskCreationOptions.RunContinuationsAsynchronously), Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
    public int Active; int entered;
    public Task Test(CancellationToken ct) => Task.CompletedTask;
    public async Task<string> Generate(string jobId, string kind, string prompt, object input, CancellationToken ct, Action<ModelActivity>? progress = null)
    {
        int current = Interlocked.Increment(ref entered); Interlocked.Increment(ref Active); if (current == 3) AllEntered.TrySetResult();
        await Release.Task.WaitAsync(ct); Interlocked.Decrement(ref Active);
        if (current == 1) throw new InvalidOperationException("模拟网络中断"); return "## 当日工作\n已完成讨论。";
    }
}
