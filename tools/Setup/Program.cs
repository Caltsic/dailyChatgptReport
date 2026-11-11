using System.Diagnostics;
using System.IO;
using System.IO.Compression;
using System.Reflection;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using Microsoft.Win32;

internal static class Program
{
    static readonly string Role = Assembly.GetExecutingAssembly().GetCustomAttributes<AssemblyMetadataAttribute>().Single(x => x.Key == "Role").Value!;
    static string Product => "Codex 工作日报 · " + (Role == "Leader" ? "组长端" : "成员端");
    static string Executable => "CodexDailyReport." + Role + ".exe";
    static string Registration => @"Software\Microsoft\Windows\CurrentVersion\Uninstall\CodexDailyReport." + Role;
    static readonly string Programs = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Programs", "CodexDailyReport");
    static string DefaultTarget()
    {
        using var key = Registry.CurrentUser.OpenSubKey(Registration);
        return key?.GetValue("InstallLocation") is string installed && Path.IsPathFullyQualified(installed) ? installed : Path.Combine(Programs, Role);
    }
    static string InstallationPath(string value)
    {
        value = Environment.ExpandEnvironmentVariables(value.Trim().Trim('"'));
        if (!Path.IsPathFullyQualified(value)) throw new IOException("请选择完整的安装目录路径");
        string target = Path.TrimEndingDirectorySeparator(Path.GetFullPath(value));
        if (target == Path.GetPathRoot(target)) throw new IOException("请在磁盘下选择一个程序文件夹");
        return target;
    }
    [STAThread]
    static int Main(string[] args)
    {
        var app = new Application();
        var target = DefaultTarget();
        // Automated installer acceptance uses an isolated directory and leaves registration untouched.
        int test = Array.IndexOf(args, "--test-root"); bool isolated = test >= 0;
        if (isolated) { if (test + 1 >= args.Length) return 2; target = Path.Combine(Path.GetFullPath(args[test + 1]), Role); }
        int destination = Array.IndexOf(args, "--install-dir");
        if (destination >= 0) { if (destination + 1 >= args.Length) return 2; target = args[destination + 1]; }
        if (args.Contains("--silent"))
        {
            try { Install(InstallationPath(target), !isolated, false); return 0; }
            catch (Exception ex) { if (isolated) File.WriteAllText(Path.Combine(Path.GetDirectoryName(target)!, "install-error.txt"), ex.ToString()); return 1; }
        }
        var window = new Window { Title = Product + " 安装", Width = 700, Height = 480, ResizeMode = ResizeMode.NoResize, WindowStartupLocation = WindowStartupLocation.CenterScreen, Background = new SolidColorBrush(Color.FromRgb(246,248,251)), FontFamily = new FontFamily("Microsoft YaHei UI"), FontSize = 14 };
        var panel = new StackPanel { Margin = new Thickness(32) };
        panel.Children.Add(new TextBlock { Text = Product, FontSize = 25, FontWeight = FontWeights.SemiBold, Foreground = new SolidColorBrush(Color.FromRgb(27,54,84)), Margin = new Thickness(0,0,0,18) });
        panel.Children.Add(new TextBlock { Text = "为当前 Windows 用户安装，可选择有写入权限的目录。\n日报数据单独保存在用户数据目录，升级与卸载不会删除。", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0,0,0,15) });
        panel.Children.Add(new TextBlock { Text = "程序安装目录", Margin = new Thickness(0,0,0,6) });
        var folder = new DockPanel { Margin = new Thickness(0,0,0,16) };
        var browse = new Button { Content = "浏览…", Padding = new Thickness(14,6,14,6), Margin = new Thickness(10,0,0,0) };
        DockPanel.SetDock(browse, Dock.Right); folder.Children.Add(browse);
        var location = new TextBox { Text = target, Padding = new Thickness(7,6,7,6), VerticalContentAlignment = VerticalAlignment.Center };
        folder.Children.Add(location); panel.Children.Add(folder);
        var startup = new CheckBox { Content = "登录 Windows 后自动启动", IsChecked = false, Margin = new Thickness(0,0,0,12) }; panel.Children.Add(startup);
        var launch = new CheckBox { Content = "安装完成后打开软件", IsChecked = true, Margin = new Thickness(0,0,0,18) }; panel.Children.Add(launch);
        var status = new TextBlock { Text = "安装前请从托盘退出已运行的同一端软件。", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0,0,0,14) }; panel.Children.Add(status);
        var install = new Button { Content = "安装 / 更新", Padding = new Thickness(20,10,20,10), HorizontalAlignment = HorizontalAlignment.Left }; panel.Children.Add(install);
        browse.Click += (_, _) =>
        {
            var dialog = new OpenFolderDialog { Title = "选择程序安装目录", InitialDirectory = Directory.Exists(location.Text) ? location.Text : Programs };
            if (dialog.ShowDialog(window) == true) location.Text = dialog.FolderName;
        };
        install.Click += async (_, _) =>
        {
            install.IsEnabled = location.IsEnabled = browse.IsEnabled = false; status.Text = "正在解压并安装，请稍候…";
            try
            {
                string selectedTarget = InstallationPath(location.Text); location.Text = selectedTarget;
                bool auto = startup.IsChecked == true;
                await Task.Run(() => Install(selectedTarget, !isolated, auto)); status.Text = "安装完成。" + (isolated ? "" : "可从开始菜单打开，或在系统应用列表中卸载。");
                if (launch.IsChecked == true) Process.Start(new ProcessStartInfo(Path.Combine(selectedTarget, Executable)) { UseShellExecute = true });
                install.Content = "已完成";
            }
            catch (Exception ex) { status.Text = "安装未完成：" + ex.Message; install.IsEnabled = location.IsEnabled = browse.IsEnabled = true; }
        };
        window.Content = panel; app.Run(window); return 0;
    }
    static void Install(string target, bool register, bool startup)
    {
        string application = Path.Combine(target, Executable);
        if (Directory.Exists(target) && Directory.EnumerateFileSystemEntries(target).Any() && !File.Exists(application))
            throw new IOException("该目录已有其他文件，请新建一个程序文件夹，或选择本软件原安装目录");
        string existingApplication = Path.Combine(register ? DefaultTarget() : target, Executable);
        foreach (var process in Process.GetProcessesByName(Path.GetFileNameWithoutExtension(Executable)))
        {
            using (process) { try { if (string.Equals(process.MainModule?.FileName, application, StringComparison.OrdinalIgnoreCase) || string.Equals(process.MainModule?.FileName, existingApplication, StringComparison.OrdinalIgnoreCase)) throw new IOException("软件仍在运行，请从托盘退出后重试"); } catch (System.ComponentModel.Win32Exception) { throw new IOException("检测到同名软件进程，请退出后重试"); } }
        }
        string parent = Path.GetDirectoryName(target)!; Directory.CreateDirectory(parent);
        string staging = target + ".new-" + Guid.NewGuid().ToString("N"); Directory.CreateDirectory(staging);
        string? previous = null;
        try
        {
            using var payload = Assembly.GetExecutingAssembly().GetManifestResourceStream("Payload.zip") ?? throw new InvalidDataException("安装包缺少内容");
            using var zip = new ZipArchive(payload, ZipArchiveMode.Read);
            foreach (var entry in zip.Entries)
            {
                string path = Path.GetFullPath(Path.Combine(staging, entry.FullName));
                if (!path.StartsWith(staging + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase)) throw new InvalidDataException("安装包路径无效");
                if (entry.FullName.EndsWith('/')) { Directory.CreateDirectory(path); continue; }
                Directory.CreateDirectory(Path.GetDirectoryName(path)!); entry.ExtractToFile(path);
            }
            if (!File.Exists(Path.Combine(staging, Executable))) throw new InvalidDataException("安装包缺少主程序");
            if (Directory.Exists(target)) { previous = target + ".previous-" + Guid.NewGuid().ToString("N"); Directory.Move(target, previous); }
            try { Directory.Move(staging, target); } catch { if (previous != null) Directory.Move(previous, target); throw; }
            File.WriteAllText(Path.Combine(target, "Uninstall.ps1"), UninstallScript(target), new UTF8Encoding(true));
            if (register)
            {
                Shortcut(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Programs), Product + ".lnk"), application);
                string startupLink = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Startup), Product + ".lnk");
                if (startup) Shortcut(startupLink, application); else if (File.Exists(startupLink)) File.Delete(startupLink);
                using var key = Registry.CurrentUser.CreateSubKey(Registration);
                key.SetValue("DisplayName", Product); key.SetValue("DisplayVersion", Assembly.GetExecutingAssembly().GetName().Version!.ToString(3)); key.SetValue("Publisher", "CodexDailyReport"); key.SetValue("InstallLocation", target); key.SetValue("DisplayIcon", application);
                key.SetValue("UninstallString", "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"" + Path.Combine(target, "Uninstall.ps1") + "\""); key.SetValue("NoModify", 1); key.SetValue("NoRepair", 1);
            }
            if (previous != null) Directory.Delete(previous, true);
        }
        finally { if (Directory.Exists(staging)) Directory.Delete(staging, true); }
    }
    static void Shortcut(string path, string target)
    {
        var type = Type.GetTypeFromProgID("WScript.Shell") ?? throw new InvalidOperationException("无法创建开始菜单快捷方式");
        dynamic shell = Activator.CreateInstance(type)!; dynamic link = shell.CreateShortcut(path); link.TargetPath = target; link.WorkingDirectory = Path.GetDirectoryName(target); link.Save();
        System.Runtime.InteropServices.Marshal.FinalReleaseComObject(link); System.Runtime.InteropServices.Marshal.FinalReleaseComObject(shell);
    }
    static string Quote(string value) => "'" + value.Replace("'", "''") + "'";
    static string UninstallScript(string target)
    {
        // Remove this installation only; an older copy cannot unregister a newer location.
        return "$ErrorActionPreference='Stop'\r\n$target=" + Quote(target) + "\r\nif ([IO.Path]::GetFullPath($target) -ne [IO.Path]::GetFullPath($PSScriptRoot)) { throw 'Unexpected installation path' }\r\n$application=Join-Path $target " + Quote(Executable) + "\r\nif (!(Test-Path -LiteralPath $application)) { throw 'Application not found in installation directory' }\r\nif (Get-Process -Name 'CodexDailyReport." + Role + "' -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $application }) { Write-Host 'Please exit the application from the system tray, then try again.'; Read-Host 'Enter'; exit 1 }\r\n" +
            "$registration='HKCU:\\" + Registration + "'\r\n$registered=(Get-ItemProperty -LiteralPath $registration -ErrorAction SilentlyContinue).InstallLocation\r\nif ($registered -and [IO.Path]::GetFullPath($registered) -eq [IO.Path]::GetFullPath($target)) {\r\n$links=@((Join-Path ([Environment]::GetFolderPath('Programs')) " + Quote(Product + ".lnk") + "),(Join-Path ([Environment]::GetFolderPath('Startup')) " + Quote(Product + ".lnk") + "))\r\nforeach($link in $links) { if(Test-Path -LiteralPath $link) { Remove-Item -LiteralPath $link -Force } }\r\nRemove-Item -LiteralPath $registration -Recurse -Force\r\n}\r\nSet-Location -LiteralPath $env:TEMP\r\nRemove-Item -LiteralPath $target -Recurse -Force\r\nWrite-Host 'Uninstalled. Daily report data was preserved.'\r\n";
    }
}
