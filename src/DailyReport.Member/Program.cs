using System.Windows;
using DailyReport.Core;
using DailyReport.Desktop;

namespace DailyReport.MemberApp;

internal static class Program
{
    [STAThread]
    static void Main(string[] args)
    {
        string root = Ui.DataDirectory("Member", args);
        using var mutex = new Mutex(true, "Local\\CodexDailyReport.Member." + Wire.Hash(root), out bool created);
        if (!created) { System.Windows.MessageBox.Show("成员端已运行，请从托盘打开。"); return; }
        var app = new Application { ShutdownMode = ShutdownMode.OnExplicitShutdown }; Ui.Initialize();
        app.DispatcherUnhandledException += (_, e) => { Ui.Error(e.Exception.Message); e.Handled = true; };
        try { app.Run(new MainWindow(root)); } catch (Exception ex) { Ui.Error(ex.Message); }
    }
}
