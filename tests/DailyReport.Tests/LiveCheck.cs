using System.IO.Compression;
using System.Net;
using System.Net.Sockets;
using DailyReport.Client;
using DailyReport.Core;
using DailyReport.Server;
using Microsoft.Data.Sqlite;

// Opt-in actual service check. Production data is opened read-only; outputs stay in an isolated copy.
internal static class LiveCheck
{
    public static async Task<int> Run(string[] args)
    {
        string production = LocalFiles.DataRoot("Leader");
        using var source = new SqliteConnection(new SqliteConnectionStringBuilder { DataSource = Path.Combine(production, "department.db"), Mode = SqliteOpenMode.ReadOnly, Pooling = false }.ToString()); source.Open();
        List<T> Read<T>()
        {
            using var cmd = source.CreateCommand(); cmd.CommandText = "SELECT json FROM entities WHERE kind=$kind"; cmd.Parameters.AddWithValue("$kind", typeof(T).Name);
            using var rows = cmd.ExecuteReader(); List<T> result = []; while (rows.Read()) result.Add(Wire.Parse<T>(rows.GetString(0))); return result;
        }
        var settings = Read<DepartmentSettings>().Single();
        int snapshotArgument = Array.IndexOf(args, "--snapshot-id");
        var snapshots = Read<Snapshot>();
        var snapshot = snapshotArgument >= 0 ? snapshots.Single(s => s.Id == args[snapshotArgument + 1]) : snapshots.MaxBy(s => s.ReceivedAt) ?? throw new Exception("没有已提交记录");
        var identity = Read<Member>().Single(m => m.Id == snapshot.MemberId);
        Capture capture;
        using (var stream = File.OpenRead(Path.Combine(production, "submissions", snapshot.FileName))) using (var zipped = new GZipStream(stream, CompressionMode.Decompress)) using (var text = new StreamReader(zipped)) capture = Wire.Parse<Capture>(text.ReadToEnd());
        int arg = Array.IndexOf(args, "--live-root");
        string root = arg >= 0 ? Path.GetFullPath(args[arg + 1]) : Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../artifacts/live", DateTime.Now.ToString("yyyyMMdd-HHmmss")));
        var store = new Store(root); var service = new DepartmentService(store);
        int port; using (var listener = new TcpListener(IPAddress.Loopback, 0)) { listener.Start(); port = ((IPEndPoint)listener.LocalEndpoint).Port; }
        service.Configure(settings with { Name = settings.Name + "（本机验证副本）", Port = port, AdvertisedUrl = $"https://localhost:{port}" });
        var employee = service.Members().FirstOrDefault() ?? service.AddMember(identity.Name + "（本机验证）");
        var documents = new Documents(root); await using var host = new DepartmentHost(service, documents); await host.Start();
        var invitation = service.Invite(employee.Id, host.Fingerprint);
        using var unpaired = new MemberConnection(invitation.Url, invitation.Fingerprint); var pairing = await unpaired.Pair(new(invitation.Code, "live-check", Wire.Id()), default);
        using var member = new MemberConnection(invitation.Url, invitation.Fingerprint, pairing.Token);
        if (store.Read(r => DepartmentService.LatestSnapshot(r, employee.Id, capture.Day)) == null)
            await member.Submit(new(Wire.Id(), Wire.Hash(Wire.Serialize(capture)), capture), default);
        using var agent = new ReportAgent(() => service.Settings, () => LocalFiles.ReadSecret(Path.Combine(production, "api-key.secret")), root);
        await using var worker = new GenerationWorker(service, agent);
        using var progressStop = new CancellationTokenSource();
        var progress = Task.Run(async () =>
        {
            string last = "";
            while (!progressStop.IsCancellationRequested)
            {
                string now = string.Join(" | ", service.Jobs(capture.Day).Where(j => j.State == "生成中").Select(j => j.Progress.Replace('\n', ' ')));
                if (now.Length > 0 && now != last) { Console.WriteLine(now); last = now; }
                try { await Task.Delay(1000, progressStop.Token); } catch (OperationCanceledException) { break; }
            }
        });
        try
        {
            Console.WriteLine($"LIVE input: {capture.SessionCount} sessions, {capture.Events.Count} events, {capture.Events.Sum(e => e.Text.Length)} text characters; output: {root}");
            var personal = service.Reports(employee.Id, capture.Day).FirstOrDefault();
            if (personal == null)
            {
                var previous = store.Read(r => r.All<Job>(capture.Day).LastOrDefault(j => j.Kind == "personal"));
                var job = previous?.State == "failed" ? service.RetryJob(previous.Id) : previous ?? service.QueueReport(employee.Id, capture.Day);
                await worker.Process(job, default); personal = service.Reports(employee.Id, capture.Day)[0];
            }
            var preview = (await member.Status(capture.Day, default)).Report ?? throw new Exception("成员未取得生成的正文");
            if (preview.Id != personal.Id) throw new Exception("成员正文版本不一致");
            if (personal.Feedback == null) await member.Feedback(new(Wire.Id(), personal.Id, "本机流程验证补充：此报告用于检查软件生成与导出流程，尚未经员工本人复核。", false), default);
            if (service.Summaries(capture.Day).Count == 0)
            {
                var previous = store.Read(r => r.All<Job>(capture.Day).LastOrDefault(j => j.Kind == "department"));
                await worker.Process(previous?.State == "failed" ? service.RetryJob(previous.Id) : previous ?? service.QueueSummary(capture.Day), default);
            }
            var summary = service.Summaries(capture.Day)[0];
            var final = service.PrepareFinalization(personal.Id); await documents.Export(final); service.CommitFinalization(final);
            await documents.Export(summary);
            string exports = Path.Combine(root, "exports"); Directory.CreateDirectory(exports);
            foreach (string extension in new[] { ".docx", ".pdf" })
            {
                File.Copy(documents.BasePath(final) + extension, Path.Combine(exports, "个人日报-验证副本" + extension), true);
                File.Copy(documents.BasePath(summary) + extension, Path.Combine(exports, "部门汇总-验证副本" + extension), true);
            }
            var downloaded = await member.Pdf(final.Id, default);
            var exported = await File.ReadAllBytesAsync(documents.BasePath(final) + ".pdf");
            if (!downloaded.SequenceEqual(exported)) throw new Exception("成员下载内容不一致");
            LocalFiles.AtomicText(Path.Combine(root, "result.json"), Wire.Serialize(new { success = true, source = new { snapshot.Id, capture.Day, capture.SessionCount, events = capture.Events.Count }, personalId = final.Id, summaryId = summary.Id, exports, at = DateTimeOffset.UtcNow }));
            Console.WriteLine("LIVE COMPLETE: " + exports); return 0;
        }
        catch (Exception ex) { Console.WriteLine("LIVE FAILED: " + ex.Message); return 1; }
        finally { await progressStop.CancelAsync(); await progress; }
    }
}
