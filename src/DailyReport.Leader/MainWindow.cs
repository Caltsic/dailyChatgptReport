using System.Diagnostics;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using System.Windows.Media;
using System.Windows.Threading;
using DailyReport.Core;
using DailyReport.Desktop;
using DailyReport.Server;
using Microsoft.Win32;

namespace DailyReport.LeaderApp;

internal sealed class MainWindow : Window
{
    readonly Store store;
    readonly DepartmentService service;
    readonly Documents documents;
    readonly CancellationTokenSource stop = new();
    DepartmentHost? host;
    GenerationWorker? worker;
    ReportAgent? gateway;
    readonly Tray tray;
    readonly DispatcherTimer timer = new() { Interval = TimeSpan.FromSeconds(2) };
    readonly DataGrid grid = new() { AutoGenerateColumns = false, IsReadOnly = true, SelectionMode = DataGridSelectionMode.Extended, CanUserAddRows = false, RowHeight = 78, HeadersVisibility = DataGridHeadersVisibility.Column, GridLinesVisibility = DataGridGridLinesVisibility.Horizontal };
    readonly DataGrid jobs = new() { AutoGenerateColumns = false, IsReadOnly = true, SelectionMode = DataGridSelectionMode.Single, CanUserAddRows = false, RowHeight = 78, HeadersVisibility = DataGridHeadersVisibility.Column };
    readonly TextBox jobDetails = new() { IsReadOnly = true, TextWrapping = TextWrapping.Wrap, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, Height = 220 };
    readonly TextBlock jobSignal = Ui.Text("", 14, true);
    readonly HashSet<string> notifiedFailures = [];
    readonly DatePicker date = new() { Width = 165, Margin = new Thickness(0, 0, 12, 8) };
    readonly TextBlock header = Ui.Text("部门工作日报", 26, true);
    readonly TextBlock status = Ui.Text("请完成部门配置", 13);
    readonly TextBlock stats = Ui.Text("", 15);
    readonly TabControl tabs = new();
    readonly TextBox department = new(), zone = new(), port = new(), address = new(), apiUrl = new(), model = new();
    readonly PasswordBox apiKey = new() { Padding = new Thickness(9) };
    bool busy, backingUp, refreshingJobs;
    public MainWindow(string root)
    {
        store = new(root); service = new(store); documents = new(root);
        Title = "Codex 工作日报 · 组长端 v" + typeof(MainWindow).Assembly.GetName().Version!.ToString(3); Width = 1400; Height = 900; MinWidth = 1000; MinHeight = 650; WindowStartupLocation = WindowStartupLocation.CenterScreen;
        Background = new SolidColorBrush(Color.FromRgb(246, 248, 251));
        var main = new DockPanel { Margin = new Thickness(28) }; var top = new StackPanel(); top.Children.Add(header); top.Children.Add(status); DockPanel.SetDock(top, Dock.Top); main.Children.Add(top);
        tabs.Items.Add(new TabItem { Header = "  当日看板  ", Content = BuildDashboard() }); tabs.Items.Add(new TabItem { Header = "  部门与模型配置  ", Content = BuildSettings() }); tabs.Items.Add(new TabItem { Header = "  Agent 执行  ", Content = BuildJobs() }); main.Children.Add(tabs); Content = main;
        notifiedFailures.UnionWith(store.Read(r => r.All<Job>().Where(j => j.State == "failed").Select(j => j.Id).ToList()));
        var settings = store.Read(r => r.Get<DepartmentSettings>("settings")); LoadSettings(settings);
        date.SelectedDate = Wire.Today(settings?.TimeZone ?? "Asia/Shanghai").ToDateTime(TimeOnly.MinValue); date.SelectedDateChanged += (_, _) => Refresh();
        tray = new(this, Shutdown); timer.Tick += async (_, _) => { Refresh(); await DailyBackup(); };
        Loaded += async (_, _) => { if (settings != null) { try { await Start(); } catch (Exception ex) { status.Text = "接收服务未启动：" + ex.Message; } } else tabs.SelectedIndex = 1; timer.Start(); Refresh(); };
    }
    UIElement BuildDashboard()
    {
        var panel = new DockPanel { Margin = new Thickness(12, 20, 12, 12) }; var controls = new StackPanel();
        var first = new WrapPanel(); first.Children.Add(date); first.Children.Add(Ui.Button("新增成员", AddMember)); first.Children.Add(Ui.Button("创建配对邀请", Invite)); first.Children.Add(Ui.Button("撤销选中设备", Revoke)); first.Children.Add(Ui.Button("刷新", () => { Refresh(); return Task.CompletedTask; })); controls.Children.Add(first);
        var actions = new WrapPanel();
        actions.Children.Add(Ui.Button("生成日报", () => Batch(r => { service.QueueReport(r.MemberId, Day); return Task.CompletedTask; }), true));
        actions.Children.Add(Ui.Button("下发报告", () => Batch(r => { service.Release(r.ReportId ?? throw new InvalidOperationException("尚无报告")); return Task.CompletedTask; })));
        actions.Children.Add(Ui.Button("要求重交", Resubmit));
        actions.Children.Add(Ui.Button("重新开启", () => Batch(r => { service.Reopen(r.MemberId, Day); return Task.CompletedTask; })));
        actions.Children.Add(Ui.Button("查看 Agent 执行", ShowJobs)); actions.Children.Add(Ui.Button("查看报告与历史", History)); actions.Children.Add(Ui.Button("导出选中报告", ExportPersonal)); controls.Children.Add(actions);
        var departmentActions = new WrapPanel(); departmentActions.Children.Add(Ui.Button("生成部门汇总", () => { service.QueueSummary(Day); Refresh(); return Task.CompletedTask; }, true)); departmentActions.Children.Add(Ui.Button("查看／导出部门汇总", Summary)); departmentActions.Children.Add(Ui.Button("备份部门数据", MakeBackup)); departmentActions.Children.Add(Ui.Button("从备份恢复到新目录", Restore)); controls.Children.Add(departmentActions);
        controls.Children.Add(Ui.Text("批量操作作用于选中成员；未选择时作用于当日全部成员。双击成员查看报告历史。", 12)); controls.Children.Add(stats); DockPanel.SetDock(controls, Dock.Top); panel.Children.Add(controls);
        foreach (var (title, binding, width) in new[] { ("成员", "Name", 100d), ("记录提交", "SubmissionStatus", 160d), ("日报进度", "ReportStatus", 140d), ("Agent 执行", "AnalysisProgress", 390d), ("记录版本", "SnapshotVersion", 75d), ("报告版本", "ReportVersion", 75d), ("个人反馈", "FeedbackStatus", 95d), ("采集截至", "CapturedAt", 230d) })
        {
            if (binding == "AnalysisProgress") { grid.Columns.Add(ProgressColumn(title, binding, width)); continue; }
            if (binding != "SubmissionStatus") { grid.Columns.Add(new DataGridTextColumn { Header = title, Binding = new Binding(binding), Width = width }); continue; }
            var row = new FrameworkElementFactory(typeof(StackPanel)); row.SetValue(StackPanel.OrientationProperty, Orientation.Horizontal); row.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
            var dot = new FrameworkElementFactory(typeof(System.Windows.Shapes.Ellipse)); dot.SetValue(FrameworkElement.WidthProperty, 10d); dot.SetValue(FrameworkElement.HeightProperty, 10d); dot.SetValue(FrameworkElement.MarginProperty, new Thickness(8, 0, 8, 0)); dot.SetBinding(System.Windows.Shapes.Shape.FillProperty, new Binding(binding) { Converter = new StatusBrushConverter() }); row.AppendChild(dot);
            var text = new FrameworkElementFactory(typeof(TextBlock)); text.SetBinding(TextBlock.TextProperty, new Binding(binding) { Converter = new StatusTextConverter() }); row.AppendChild(text);
            grid.Columns.Add(new DataGridTemplateColumn { Header = title, Width = width, CellTemplate = new DataTemplate { VisualTree = row } });
        }
        grid.MouseDoubleClick += async (_, _) => { try { await History(); } catch (Exception ex) { Ui.Error(ex.Message); } }; panel.Children.Add(grid); return panel;
    }
    static DataGridTemplateColumn ProgressColumn(string title, string binding, double width)
    {
        var panel = new FrameworkElementFactory(typeof(StackPanel)); panel.SetValue(FrameworkElement.MarginProperty, new Thickness(5)); panel.SetBinding(FrameworkElement.ToolTipProperty, new Binding(binding));
        var text = new FrameworkElementFactory(typeof(TextBlock)); text.SetValue(TextBlock.TextWrappingProperty, TextWrapping.Wrap); text.SetValue(FrameworkElement.MaxHeightProperty, 52d); text.SetBinding(TextBlock.TextProperty, new Binding(binding)); panel.AppendChild(text);
        return new DataGridTemplateColumn { Header = title, Width = width, CellTemplate = new DataTemplate { VisualTree = panel } };
    }
    UIElement BuildJobs()
    {
        var panel = new DockPanel { Margin = new Thickness(12, 20, 12, 12) };
        var top = new StackPanel(); top.Children.Add(Ui.Text("显示所选日期的个人日报与部门汇总执行记录，包括当前动作、接收正文和错误信息。", 13)); top.Children.Add(jobSignal);
        var buttons = new WrapPanel(); buttons.Children.Add(Ui.Button("刷新", () => { Refresh(); return Task.CompletedTask; })); buttons.Children.Add(Ui.Button("重试选中失败任务", () => { var row = jobs.SelectedItem as JobView ?? throw new InvalidOperationException("请先选择失败任务"); service.RetryJob(row.Id); Refresh(); return Task.CompletedTask; }, true)); top.Children.Add(buttons); DockPanel.SetDock(top, Dock.Top); panel.Children.Add(top);
        DockPanel.SetDock(jobDetails, Dock.Bottom); panel.Children.Add(jobDetails);
        foreach (var (title, binding, width) in new[] { ("成员／部门", "Owner", 105d), ("任务", "Kind", 95d), ("状态", "State", 80d), ("当前阶段与模型状态", "Progress", 550d), ("耗时", "Elapsed", 110d), ("异常提示", "Signal", 100d) })
            jobs.Columns.Add(binding == "Progress" ? ProgressColumn(title, binding, width) : new DataGridTextColumn { Header = title, Binding = new Binding(binding), Width = width });
        jobs.SelectionChanged += (_, _) => { if (!refreshingJobs) UpdateJobDetails(true); }; panel.Children.Add(jobs); return panel;
    }
    Task ShowJobs()
    {
        string? id = (grid.SelectedItem as DashboardRow)?.JobId; tabs.SelectedIndex = 2; Refresh();
        if (id != null) jobs.SelectedItem = jobs.Items.Cast<JobView>().FirstOrDefault(j => j.Id == id);
        return Task.CompletedTask;
    }
    void UpdateJobDetails(bool resetScroll = false)
    {
        string text = (jobs.SelectedItem as JobView)?.Details ?? "选择任务查看 Agent 执行记录。";
        if (jobDetails.Text != text) { double offset = resetScroll ? 0 : jobDetails.VerticalOffset; jobDetails.Text = text; jobDetails.ScrollToVerticalOffset(offset); }
    }
    UIElement BuildSettings()
    {
        var panel = new StackPanel { Margin = new Thickness(24), MaxWidth = 780, HorizontalAlignment = HorizontalAlignment.Left };
        panel.Children.Add(Ui.Text("本部门独立收集与生成", 21, true));
        foreach (var (label, input) in new (string, UIElement)[] { ("部门名称", department), ("时区（已有记录后固定）", zone), ("HTTPS 接收端口", port), ("成员可访问的固定 HTTPS 地址，例如 https://192.168.1.10:18443", address), ("模型 API Base URL，例如 https://api.example.com/v1", apiUrl), ("API Key（留空保留现有密钥）", apiKey), ("模型名称", model) }) panel.Children.Add(Ui.Field(label, input));
        var buttons = new WrapPanel(); buttons.Children.Add(Ui.Button("保存并启动接收", SaveSettings, true)); buttons.Children.Add(Ui.Button("测试已保存模型配置", TestModel)); buttons.Children.Add(Ui.Button("打开数据目录", () => { Ui.Open(store.Root); return Task.CompletedTask; })); panel.Children.Add(buttons);
        panel.Children.Add(Ui.Text("提示词随软件版本发布，不提供编辑入口。API Key 仅在当前 Windows 用户下保存。\n成员仅向该部门提交记录；本端会将提交内容发往以上模型服务。\n请为接收端口配置内网／VPN 防火墙规则。组长端关闭窗口后仍在托盘接收，明确退出或关机后暂停。", 12));
        return new ScrollViewer { Content = panel, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
    }
    void LoadSettings(DepartmentSettings? s)
    {
        department.Text = s?.Name ?? ""; zone.Text = s?.TimeZone ?? "Asia/Shanghai"; port.Text = (s?.Port ?? 18443).ToString(); address.Text = s?.AdvertisedUrl ?? $"https://{Environment.MachineName}:18443"; apiUrl.Text = s?.ApiBaseUrl ?? ""; model.Text = s?.Model ?? "";
    }
    string Day => (date.SelectedDate ?? DateTime.Today).ToString("yyyy-MM-dd");
    List<DashboardRow> Targets() => grid.SelectedItems.Cast<DashboardRow>().ToList() is { Count: > 0 } selected ? selected : service.Dashboard(Day);
    DashboardRow Selected() => grid.SelectedItem as DashboardRow ?? throw new InvalidOperationException("请先选择一个成员");
    async Task SaveSettings()
    {
        var old = store.Read(r => r.Get<DepartmentSettings>("settings"));
        var settings = new DepartmentSettings(old?.Id ?? Wire.Id(), department.Text.Trim(), zone.Text.Trim(), int.Parse(port.Text), address.Text.Trim(), apiUrl.Text.Trim(), model.Text.Trim(), old?.ChunkCharacters ?? 18000);
        if (store.Read(r => r.All<Job>().Any(j => j.State is "queued" or "running"))) throw new InvalidOperationException("有生成任务未结束，请稍后修改配置");
        service.Configure(settings, apiKey.Password); apiKey.Clear();
        await StopServices(); await Start(); tabs.SelectedIndex = 0; Refresh();
    }
    async Task Start()
    {
        host = new(service, documents); await host.Start(stop.Token);
        gateway = new(() => service.Settings, () => File.Exists(Path.Combine(store.Root, "api-key.secret")) ? LocalFiles.ReadSecret(Path.Combine(store.Root, "api-key.secret")) : "", store.Root);
        worker = new(service, gateway); worker.Start(); status.Text = "接收服务运行中 · " + service.Settings.AdvertisedUrl + " · 关闭窗口后继续在托盘运行";
        await DailyBackup();
    }
    async Task StopServices() { if (worker != null) { await worker.DisposeAsync(); worker = null; } if (host != null) { await host.DisposeAsync(); host = null; } gateway?.Dispose(); gateway = null; }
    async Task TestModel() { if (gateway == null) throw new InvalidOperationException("请先保存并启动接收"); await gateway.Test(stop.Token); Ui.Info("模型连接测试成功。"); }
    void Refresh()
    {
        if (store.Read(r => r.Get<DepartmentSettings>("settings")) == null) return;
        var ids = grid.SelectedItems.Cast<DashboardRow>().Select(x => x.MemberId).ToHashSet();
        var rows = service.Dashboard(Day); grid.ItemsSource = rows; foreach (var row in rows.Where(r => ids.Contains(r.MemberId))) grid.SelectedItems.Add(row);
        header.Text = service.Settings.Name + " · 工作日报";
        var summary = service.Summaries(Day).FirstOrDefault();
        var summaryJob = store.Read(r => r.All<Job>(Day).Where(j => j.Kind == "department").OrderByDescending(j => j.State is "queued" or "running").ThenByDescending(j => j.Progress?.UpdatedAt ?? j.CreatedAt).FirstOrDefault());
        stats.Text = $"成员 {rows.Count}　已收到 {rows.Count(r => r.SnapshotVersion > 0)}　已定稿 {rows.Count(r => r.ReportStatus == "已定稿")}　待反馈 {rows.Count(r => r.ReportStatus == "待反馈")}\n部门汇总：" + (summaryJob?.State is "queued" or "running" ? JobProgressText.Brief(summaryJob) : summaryJob?.State == "failed" ? "失败：" + summaryJob.Error : summary == null ? "尚未生成" : $"V{summary.Version}" + (service.IsSummaryStale(summary) ? " · 来源已更新，需重新生成" : " · 来源有效"));
        string? selected = (jobs.SelectedItem as JobView)?.Id;
        var taskRows = service.Jobs(Day);
        refreshingJobs = true;
        try { jobs.ItemsSource = taskRows; jobs.SelectedItem = taskRows.FirstOrDefault(j => j.Id == selected) ?? taskRows.FirstOrDefault(); }
        finally { refreshingJobs = false; }
        UpdateJobDetails(selected != (jobs.SelectedItem as JobView)?.Id);
        jobSignal.Text = $"{Day}　生成中 {taskRows.Count(j => j.State == "生成中")}　排队 {taskRows.Count(j => j.State == "排队中")}　失败记录 {taskRows.Count(j => j.State == "失败")}　响应较慢 {taskRows.Count(j => j.Signal == "响应较慢")}";
        foreach (var failed in taskRows.Where(j => j.State == "失败")) if (notifiedFailures.Add(failed.Id)) tray.Notify(failed.Owner + " · " + failed.Kind + "生成失败，请在分析任务中查看原因。");
    }
    Task AddMember() { var name = Ui.Input(this, "新增成员", "成员姓名（部门内唯一）"); if (name != null) { service.AddMember(name); Refresh(); } return Task.CompletedTask; }
    Task Invite()
    {
        var member = Selected(); if (host == null) throw new InvalidOperationException("接收服务未运行");
        var invite = service.Invite(member.MemberId, host.Fingerprint); string value = Convert.ToBase64String(Encoding.UTF8.GetBytes(Wire.Serialize(invite)));
        System.Windows.Clipboard.SetText(value); Ui.Input(this, "邀请已复制", $"为 {member.Name} 创建，30 分钟内有效。请通过可信渠道发送完整邀请。", value, true); return Task.CompletedTask;
    }
    Task Revoke() { service.Revoke(Selected().MemberId); Ui.Info("该成员设备已撤销，后续请求将被拒绝。可创建新邀请重新配对。"); return Task.CompletedTask; }
    async Task Resubmit()
    {
        var reason = Ui.Input(this, "要求重新提交", "填写原因；已定稿的成员需先重新开启。", multiline: true); if (reason == null) return;
        await Batch(r => { service.RequestResubmit(r.MemberId, Day, reason); return Task.CompletedTask; });
    }
    async Task Batch(Func<DashboardRow, Task> action)
    {
        if (busy) throw new InvalidOperationException("正在处理另一批操作"); busy = true; List<string> errors = []; int completed = 0;
        try { await Task.WhenAll(Targets().Select(async row => { try { await action(row); completed++; } catch (Exception ex) { errors.Add(row.Name + "：" + ex.Message); } })); }
        finally { busy = false; Refresh(); }
        status.Text = $"本次操作已处理 {completed} 人" + (errors.Count == 0 ? "" : $"，{errors.Count} 人未完成");
        if (errors.Count > 0) Ui.Info(string.Join("\n", errors));
    }
    Task History()
    {
        var row = Selected(); var reports = service.Reports(row.MemberId, Day); if (reports.Count == 0) throw new InvalidOperationException("尚无报告");
        var dialog = new Window { Owner = this, Title = row.Name + " · 日报版本", Width = 920, Height = 760, WindowStartupLocation = WindowStartupLocation.CenterOwner };
        var panel = new DockPanel { Margin = new Thickness(20) }; var selector = new ComboBox { ItemsSource = reports.Select(r => new { Report = r, Label = $"V{r.Version} / 来源 V{r.SnapshotVersion} / {(r.FinalizedAt != null ? "已定稿" : r.ReleasedAt != null ? "已下发" : "待下发")} / {r.CreatedAt.LocalDateTime:g}" }).ToList(), DisplayMemberPath = "Label", Margin = new Thickness(0, 0, 0, 12) }; DockPanel.SetDock(selector, Dock.Top); panel.Children.Add(selector);
        var text = new TextBox { IsReadOnly = true, TextWrapping = TextWrapping.Wrap, VerticalScrollBarVisibility = ScrollBarVisibility.Auto }; panel.Children.Add(text); selector.SelectionChanged += (_, _) => { if (selector.SelectedIndex >= 0) text.Text = ReportText.Preview(reports[selector.SelectedIndex]); }; selector.SelectedIndex = 0;
        dialog.Content = panel; dialog.ShowDialog(); return Task.CompletedTask;
    }
    async Task ExportPersonal()
    {
        var row = Selected(); var report = service.PrepareFinalization(row.ReportId ?? throw new InvalidOperationException("尚无报告"));
        await documents.Export(report, stop.Token); service.CommitFinalization(report); Refresh();
        await CopyExport(documents.BasePath(report), $"{report.Day}-{row.Name}-V{report.Version}");
    }
    Task Summary()
    {
        var reports = service.Summaries(Day); if (reports.Count == 0) throw new InvalidOperationException("尚无部门汇总");
        var dialog = new Window { Owner = this, Title = "部门当日汇总", Width = 920, Height = 760, WindowStartupLocation = WindowStartupLocation.CenterOwner };
        var panel = new DockPanel { Margin = new Thickness(20) }; var selector = new ComboBox { ItemsSource = reports.Select(r => $"V{r.Version} / {r.CreatedAt.LocalDateTime:g}" + (service.IsSummaryStale(r) ? " / 来源已更新" : "")).ToList(), Margin = new Thickness(0, 0, 0, 12) }; DockPanel.SetDock(selector, Dock.Top); panel.Children.Add(selector);
        var export = Ui.Button("导出 PDF 与 Word", async () => { var r = reports[selector.SelectedIndex]; if (service.IsSummaryStale(r)) throw new InvalidOperationException("汇总来源已更新，请重新生成后导出"); await documents.Export(r, stop.Token); await CopyExport(documents.BasePath(r), $"{Day}-部门汇总-V{r.Version}"); }); DockPanel.SetDock(export, Dock.Bottom); panel.Children.Add(export);
        var text = new TextBox { IsReadOnly = true, TextWrapping = TextWrapping.Wrap, VerticalScrollBarVisibility = ScrollBarVisibility.Auto }; panel.Children.Add(text); selector.SelectionChanged += (_, _) => { if (selector.SelectedIndex >= 0) text.Text = ReportText.Preview(reports[selector.SelectedIndex]); }; selector.SelectedIndex = 0; dialog.Content = panel; dialog.ShowDialog(); return Task.CompletedTask;
    }
    Task CopyExport(string source, string name)
    {
        foreach (char bad in Path.GetInvalidFileNameChars()) name = name.Replace(bad, '_');
        var dialog = new SaveFileDialog { Filter = "PDF 正式文档（同时导出 Word）|*.pdf", FileName = name + ".pdf" };
        if (dialog.ShowDialog(this) == true) { File.Copy(source + ".pdf", dialog.FileName, true); File.Copy(source + ".docx", Path.ChangeExtension(dialog.FileName, ".docx"), true); Ui.Open(Path.GetDirectoryName(dialog.FileName)!); } return Task.CompletedTask;
    }
    async Task MakeBackup()
    {
        var dialog = new OpenFolderDialog { Title = "选择备份保存文件夹" }; if (dialog.ShowDialog(this) != true) return;
        var file = await Task.Run(() => Backup.Create(store, dialog.FolderName)); Ui.Info("备份已完成：\n" + file + "\n备份不包含 API Key 和可用设备凭据。");
    }
    async Task Restore()
    {
        var archive = new OpenFileDialog { Filter = "部门备份|*.zip" }; if (archive.ShowDialog(this) != true) return;
        var destination = new OpenFolderDialog { Title = "选择空文件夹作为恢复目标（不会覆盖当前数据）" }; if (destination.ShowDialog(this) != true) return;
        await Task.Run(() => Backup.Restore(archive.FileName, destination.FolderName));
        var launch = new ProcessStartInfo(Environment.ProcessPath!) { UseShellExecute = true }; launch.ArgumentList.Add("--data-dir"); launch.ArgumentList.Add(destination.FolderName);
        Ui.Info("数据已恢复。当前接收服务将停止，请在恢复后的组长端重新配置 API Key、地址和设备配对。今后从恢复目录中的快捷方式启动。\n" + destination.FolderName);
        dynamic shell = Activator.CreateInstance(Type.GetTypeFromProgID("WScript.Shell")!)!;
        dynamic shortcut = shell.CreateShortcut(Path.Combine(destination.FolderName, "启动组长端.lnk"));
        shortcut.TargetPath = Environment.ProcessPath; shortcut.Arguments = "--data-dir \"" + destination.FolderName + "\""; shortcut.WorkingDirectory = destination.FolderName; shortcut.Save();
        System.Runtime.InteropServices.Marshal.FinalReleaseComObject(shortcut); System.Runtime.InteropServices.Marshal.FinalReleaseComObject(shell);
        await StopServices(); Process.Start(launch);
    }
    async Task DailyBackup()
    {
        if (backingUp || store.Read(r => r.Get<DepartmentSettings>("settings")) == null) return;
        backingUp = true; try { await Task.Run(() => Backup.Daily(store)); } catch (Exception) { status.Text = "自动备份未完成，请检查存储空间并手动备份"; } finally { backingUp = false; }
    }
    async Task Shutdown() { timer.Stop(); await stop.CancelAsync(); await StopServices(); tray.Dispose(); }
}
