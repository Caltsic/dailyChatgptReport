using System.Text;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Threading;
using DailyReport.Client;
using DailyReport.Core;
using DailyReport.Desktop;
using Microsoft.Win32;

namespace DailyReport.MemberApp;

internal sealed class MainWindow : Window
{
    readonly MemberRepository repository;
    readonly MemberSync sync;
    readonly CancellationTokenSource stop = new();
    readonly DatePicker date = new() { Width = 160, Margin = new Thickness(0, 0, 12, 8) };
    readonly TextBlock identity = Ui.Text("尚未绑定部门", 15);
    readonly TextBlock status = Ui.Text("首次使用请绑定部门", 14);
    readonly TextBox preview = new() { Text = "组长生成并下发后，报告正文将在这里显示。", IsReadOnly = true, AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, MinHeight = 200 };
    readonly TextBox supplement = new() { AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, Height = 110 };
    readonly Tray tray;
    readonly DispatcherTimer timer = new() { Interval = TimeSpan.FromSeconds(15) };
    Report? displayed;
    bool changing, refreshing;
    string lastNotice = "";
    MemberStatus? lastStatus;
    public MainWindow(string root)
    {
        repository = new(root); sync = new(repository);
        Title = "Codex 工作日报 · 成员端 v" + typeof(MainWindow).Assembly.GetName().Version!.ToString(3); Width = 880; Height = 850; MinWidth = 700; MinHeight = 650; WindowStartupLocation = WindowStartupLocation.CenterScreen;
        Background = new System.Windows.Media.SolidColorBrush(System.Windows.Media.Color.FromRgb(246, 248, 251));
        var main = new Grid { Margin = new Thickness(28) };
        main.RowDefinitions.Add(new() { Height = GridLength.Auto }); main.RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) }); main.RowDefinitions.Add(new() { Height = GridLength.Auto });
        var top = new StackPanel(); top.Children.Add(Ui.Text("今日工作，留下一份清晰记录", 24, true)); top.Children.Add(identity);
        var controls = new WrapPanel(); controls.Children.Add(date); controls.Children.Add(Ui.Button("提交记录", Submit, true)); controls.Children.Add(Ui.Button("刷新状态", Refresh)); controls.Children.Add(Ui.Button("绑定部门", Pair)); controls.Children.Add(Ui.Button("记录路径", ConfigureRoot)); controls.Children.Add(Ui.Button("查看重交要求", ShowNotices)); controls.Children.Add(Ui.Button("采集说明", () => { Ui.Info(string.Join("\n", (lastStatus?.CaptureWarnings ?? []).Take(30)) is { Length: > 0 } s ? s : "尚无采集说明"); return Task.CompletedTask; })); top.Children.Add(controls); top.Children.Add(status); main.Children.Add(top);
        Grid.SetRow(preview, 1); main.Children.Add(preview);
        var bottom = new StackPanel { Margin = new Thickness(0, 18, 0, 0) }; bottom.Children.Add(Ui.Text("个人补充说明", 16, true)); bottom.Children.Add(Ui.Text("复核正文后填写补充；没有补充请填写“无补充说明”。提交内容按原文附在报告末尾。", 12)); bottom.Children.Add(supplement);
        var actions = new WrapPanel { Margin = new Thickness(0, 12, 0, 0) }; actions.Children.Add(Ui.Button("保存草稿", () => { SaveDraft(); status.Text = "草稿已保存在本机"; return Task.CompletedTask; })); actions.Children.Add(Ui.Button("提交补充", () => Feedback(false), true)); actions.Children.Add(Ui.Button("确认无补充", () => Feedback(true))); actions.Children.Add(Ui.Button("下载正式 PDF", Download)); actions.Children.Add(Ui.Button("查看待发送项", ShowOutbox)); bottom.Children.Add(actions); Grid.SetRow(bottom, 2); main.Children.Add(bottom); Content = main;
        date.SelectedDate = Wire.Today(repository.Pairing?.Identity.TimeZone ?? "Asia/Shanghai").ToDateTime(TimeOnly.MinValue);
        date.SelectedDateChanged += async (_, _) => { SaveDraft(); Display(null, null); await Refresh(); };
        supplement.TextChanged += (_, _) => { if (!changing) SaveDraft(); };
        tray = new(this, Shutdown); timer.Tick += async (_, _) => await Refresh(); timer.Start();
        Loaded += async (_, _) => await Refresh();
    }
    string Day => (date.SelectedDate ?? DateTime.Today).ToString("yyyy-MM-dd");
    string Root => File.Exists(Path.Combine(repository.Root, "codex-root.txt")) ? File.ReadAllText(Path.Combine(repository.Root, "codex-root.txt")) : CodexCollector.DefaultRoot;
    async Task Pair()
    {
        string? text = Ui.Input(this, "绑定部门", "粘贴组长提供的配对邀请（有效期 30 分钟）", multiline: true); if (string.IsNullOrWhiteSpace(text)) return;
        Invitation invitation;
        try { invitation = Wire.Parse<Invitation>(Encoding.UTF8.GetString(Convert.FromBase64String(text.Trim()))); } catch { throw new InvalidDataException("邀请格式无效，请复制完整的邀请内容"); }
        if (invitation.Protocol != Wire.Protocol) throw new InvalidDataException("邀请版本不兼容，请升级成员端");
        var consent = MessageBox.Show($"部门：{invitation.Department}\n连接地址：{invitation.Url}\n模型处理地址：{invitation.ModelDestination}\n时区：{invitation.TimeZone}\n证书指纹：{invitation.Fingerprint}\n\n提交内容包括用户与助手可见对话、工具动作及结果，可能包含代码。不会主动读取项目文件、附件和登录配置。凭据脱敏不能保证覆盖所有秘密。\n\n请核对邀请来自可信组长。是否同意将这些内容交给部门及其模型服务处理？", "确认数据范围及部门身份", MessageBoxButton.YesNo, MessageBoxImage.Information);
        if (consent != MessageBoxResult.Yes) return;
        using var client = new MemberConnection(invitation.Url, invitation.Fingerprint);
        // Persist request ID before sending so a lost pairing reply can be safely retried.
        var path = Path.Combine(repository.Root, "pair-request-" + Wire.Hash(invitation.Code) + ".txt");
        var id = File.Exists(path) ? File.ReadAllText(path) : Wire.Id(); LocalFiles.AtomicText(path, id);
        var reply = await client.Pair(new(invitation.Code, Environment.MachineName, id), stop.Token);
        repository.Bind(new(invitation.Url, invitation.Fingerprint, reply)); await Refresh();
    }
    Task ConfigureRoot()
    {
        var value = Ui.Input(this, "Codex 记录路径", "只扫描该目录下的 sessions 和 archived_sessions。", Root);
        if (value != null) { if (!Directory.Exists(value)) throw new DirectoryNotFoundException("该目录不存在"); LocalFiles.AtomicText(Path.Combine(repository.Root, "codex-root.txt"), Path.GetFullPath(value)); }
        return Task.CompletedTask;
    }
    async Task Submit()
    {
        var p = repository.Pairing ?? throw new InvalidOperationException("请先绑定部门");
        string selectedDay = Day, root = Root; status.Text = "正在采集记录…";
        var capture = await Task.Run(() => new CodexCollector().Collect(root, DateOnly.Parse(selectedDay), p.Identity.TimeZone, stop.Token));
        repository.Enqueue(new Upload(Wire.Id(), Wire.Hash(Wire.Serialize(capture)), capture));
        status.Text = $"已采集 {capture.SessionCount} 个会话，{capture.Events.Count} 条事件，等待部门端接收";
        await Refresh();
    }
    async Task Refresh()
    {
        if (refreshing) return; refreshing = true;
        try
        {
            var p = repository.Pairing; if (p == null) return;
            identity.Text = p.Identity.DepartmentName + " / " + p.Identity.MemberName;
            string day = Day; string queue = await sync.Flush(stop.Token);
            using var client = new MemberConnection(p.Url, p.Fingerprint, p.Identity.Token);
            try
            {
                var result = await client.Status(day, stop.Token);
                if (day != Day) return;
                var cached = repository.Cached(day); repository.Cache(result); lastStatus = result;
                status.Text = $"{result.SubmissionStatus}　{result.ReportStatus}\n{queue}";
                if (result.CapturedAt != null) status.Text += $"\n采集截至 {TimeZoneInfo.ConvertTime(result.CapturedAt.Value, TimeZoneInfo.FindSystemTimeZoneById(result.TimeZone)):yyyy-MM-dd HH:mm:ss} · {result.SessionCount} 个会话 · 范围说明 {result.CaptureWarnings?.Count ?? 0} 条";
                var pending = repository.Items().Where(x => x.Error != null).ToList(); if (pending.Count > 0) status.Text += $"\n{pending.Count} 项需要处理，请查看待发送项。";
                if (result.ModelDestination != p.Identity.ModelDestination) status.Text += "\n注意：部门模型地址已变更为 " + result.ModelDestination;
                Display(result.Report, cached?.Report);
                var notice = result.Notices.LastOrDefault();
                if (notice != null) { status.Text += "\n重交要求（" + notice.Day + "）：" + notice.Reason; if (lastNotice != notice.Id) { lastNotice = notice.Id; tray.Notify("组长要求重新提交 " + notice.Day + "：" + notice.Reason); } }
            }
            catch (Exception ex) when (ex is HttpRequestException or TaskCanceledException or ApiException)
            { status.Text = "当前未连接部门端；显示本机缓存。\n" + queue + (ex is ApiException ? "\n" + ex.Message : ""); Display(repository.Cached(day)?.Report, null); }
        }
        catch (Exception ex) when (!stop.IsCancellationRequested) { status.Text = "同步未完成：" + ex.Message; }
        finally { refreshing = false; }
    }
    void Display(Report? report, Report? previous)
    {
        if (report?.Id != displayed?.Id)
        {
            SaveDraft(); changing = true;
            string draft = report == null ? "" : repository.Draft(report.Id);
            var old = displayed?.Day == report?.Day ? displayed : previous;
            if (report != null && draft.Length == 0 && old != null && old.Id != report.Id) { draft = repository.Draft(old.Id); if (draft.Length == 0) draft = old.Feedback?.Text ?? ""; if (draft.Length > 0) status.Text += "\n已保留上一版补充为草稿，请核对新正文后重新提交。"; }
            displayed = report; supplement.Text = draft.Length > 0 ? draft : report?.Feedback?.Text ?? ""; changing = false;
        }
        else displayed = report;
        preview.Text = report == null ? "组长生成并下发后，报告正文将在这里显示。" : ReportText.Preview(report);
        supplement.IsReadOnly = report?.FinalizedAt != null;
    }
    void SaveDraft() { if (displayed != null && !changing) repository.SaveDraft(displayed.Id, supplement.Text); }
    Task ShowNotices()
    {
        var notices = lastStatus?.Notices ?? [];
        if (notices.Count == 0) { Ui.Info("没有未完成的重交要求。"); return Task.CompletedTask; }
        var dialog = new Window { Owner = this, Title = "重交要求", Width = 620, Height = 330, WindowStartupLocation = WindowStartupLocation.CenterOwner };
        var panel = new DockPanel { Margin = new Thickness(20) }; var list = new ListBox { ItemsSource = notices.Select(n => n.Day + "：" + n.Reason).ToList() };
        var button = Ui.Button("切换到选中日期", () => { if (list.SelectedIndex >= 0) { date.SelectedDate = DateOnly.Parse(notices[list.SelectedIndex].Day).ToDateTime(TimeOnly.MinValue); dialog.Close(); } return Task.CompletedTask; }); DockPanel.SetDock(button, Dock.Bottom); panel.Children.Add(button); panel.Children.Add(list); dialog.Content = panel; dialog.ShowDialog(); return Task.CompletedTask;
    }
    async Task Feedback(bool none)
    {
        var report = displayed ?? throw new InvalidOperationException("尚无可补充的报告");
        if (report.Day != Day) throw new InvalidOperationException("日期已变更，请先刷新报告");
        if (report.FinalizedAt != null) throw new InvalidOperationException("报告已定稿，请联系组长重新开启并生成新版");
        if (none) supplement.Text = "无补充说明";
        if (string.IsNullOrWhiteSpace(supplement.Text)) throw new InvalidOperationException("请填写补充；没有补充请填写“无补充说明”");
        SaveDraft(); repository.Enqueue(new SupplementRequest(Wire.Id(), report.Id, supplement.Text, supplement.Text.Trim() == "无补充说明")); await Refresh();
    }
    async Task Download()
    {
        var report = displayed ?? throw new InvalidOperationException("尚无报告"); if (report.Day != Day) throw new InvalidOperationException("日期已变更，请先刷新报告"); if (report.FinalizedAt == null) throw new InvalidOperationException("报告尚未定稿");
        var dialog = new SaveFileDialog { Filter = "PDF 文档|*.pdf", FileName = report.Day + "-工作日报-V" + report.Version + ".pdf" }; if (dialog.ShowDialog(this) != true) return;
        var p = repository.Pairing!; using var client = new MemberConnection(p.Url, p.Fingerprint, p.Identity.Token); var bytes = await client.Pdf(report.Id, stop.Token); await File.WriteAllBytesAsync(dialog.FileName, bytes, stop.Token); Ui.Open(dialog.FileName);
    }
    Task ShowOutbox()
    {
        var items = repository.Items();
        if (items.Count == 0) { Ui.Info("没有待发送内容。"); return Task.CompletedTask; }
        var dialog = new Window { Owner = this, Title = "待发送项", Width = 760, Height = 380, WindowStartupLocation = WindowStartupLocation.CenterOwner };
        var panel = new DockPanel { Margin = new Thickness(20) }; var list = new ListBox { DisplayMemberPath = "Display", ItemsSource = items.Select(i => new { Item = i, Display = $"{i.CreatedAt.LocalDateTime:g} {(i.Kind == "upload" ? "工作记录" : "个人补充")}：{i.Error ?? "等待发送"}" }).ToList() };
        var remove = Ui.Button("移除选中项（保留补充草稿）", () => { if (list.SelectedItem is { } selected) { var item = (OutboxItem)selected.GetType().GetProperty("Item")!.GetValue(selected)!; repository.Complete(item.Id); dialog.Close(); } return Task.CompletedTask; }); DockPanel.SetDock(remove, Dock.Bottom); panel.Children.Add(remove); panel.Children.Add(list); dialog.Content = panel; dialog.ShowDialog(); return Task.CompletedTask;
    }
    async Task Shutdown() { timer.Stop(); SaveDraft(); await stop.CancelAsync(); tray.Dispose(); }
}
