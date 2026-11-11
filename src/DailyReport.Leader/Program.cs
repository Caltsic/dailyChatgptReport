using System.Windows;
using DailyReport.Core;
using DailyReport.Desktop;
using DailyReport.Server;

namespace DailyReport.LeaderApp;

internal static class Program
{
    [STAThread]
    static void Main(string[] args)
    {
        string root = Ui.DataDirectory("Leader", args);
        using var mutex = new Mutex(true, "Local\\CodexDailyReport.Leader." + Wire.Hash(root), out bool created);
        if (!created) { MessageBox.Show("组长端已运行，请从托盘打开。"); return; }
        var app = new Application { ShutdownMode = ShutdownMode.OnExplicitShutdown }; Ui.Initialize();
        app.DispatcherUnhandledException += (_, e) => { Ui.Error(e.Exception.Message); e.Handled = true; };
        try
        {
            int restore = Array.IndexOf(args, "--restore");
            if (restore >= 0 && restore + 1 < args.Length) Backup.Restore(args[restore + 1], root);
            app.Run(new MainWindow(root));
        }
        catch (Exception ex) { Ui.Error(ex.Message); }
    }
}
