using System.Diagnostics;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Data;
using System.Globalization;
using DailyReport.Core;
using Forms = System.Windows.Forms;

namespace DailyReport.Desktop;

internal static class Ui
{
    public static readonly Brush Navy = new SolidColorBrush(Color.FromRgb(27, 54, 84));
    public static readonly Brush Muted = new SolidColorBrush(Color.FromRgb(87, 105, 120));
    public static void Initialize()
    {
        var r = System.Windows.Application.Current.Resources;
        var control = new Style(typeof(Control)); control.Setters.Add(new Setter(Control.FontFamilyProperty, new FontFamily("Microsoft YaHei UI"))); control.Setters.Add(new Setter(Control.FontSizeProperty, 14d)); r.Add(typeof(Control), control);
        var button = new Style(typeof(Button), control); button.Setters.Add(new Setter(Control.PaddingProperty, new Thickness(14, 8, 14, 8))); button.Setters.Add(new Setter(FrameworkElement.MarginProperty, new Thickness(0, 0, 8, 8))); button.Setters.Add(new Setter(Control.BackgroundProperty, new SolidColorBrush(Color.FromRgb(235, 241, 248)))); button.Setters.Add(new Setter(Control.ForegroundProperty, Navy)); r.Add(typeof(Button), button);
        var box = new Style(typeof(TextBox), control); box.Setters.Add(new Setter(Control.PaddingProperty, new Thickness(9))); box.Setters.Add(new Setter(Control.BorderBrushProperty, new SolidColorBrush(Color.FromRgb(204, 215, 225)))); r.Add(typeof(TextBox), box);
    }
    public static TextBlock Text(string text, double size = 14, bool bold = false) => new() { Text = text, FontSize = size, FontWeight = bold ? FontWeights.SemiBold : FontWeights.Normal, TextWrapping = TextWrapping.Wrap, Foreground = Navy, Margin = new Thickness(0, 0, 0, 10) };
    public static Button Button(string text, Func<Task> action, bool primary = false)
    {
        var button = new Button { Content = text };
        if (primary) { button.Background = Navy; button.Foreground = Brushes.White; }
        button.Click += async (_, _) => { button.IsEnabled = false; try { await action(); } catch (Exception ex) { Error(ex.Message); } finally { button.IsEnabled = true; } }; return button;
    }
    public static void Error(string text) => System.Windows.MessageBox.Show(text, "操作未完成", MessageBoxButton.OK, MessageBoxImage.Warning);
    public static void Info(string text) => System.Windows.MessageBox.Show(text, "Codex 部门日报", MessageBoxButton.OK, MessageBoxImage.Information);
    public static string? Input(Window owner, string title, string description, string value = "", bool multiline = false)
    {
        var dialog = new Window { Title = title, Owner = owner, Width = 560, Height = multiline ? 350 : 230, WindowStartupLocation = WindowStartupLocation.CenterOwner, ResizeMode = ResizeMode.NoResize };
        var panel = new StackPanel { Margin = new Thickness(24) }; panel.Children.Add(Text(description));
        var box = new TextBox { Text = value, Height = multiline ? 150 : 40, AcceptsReturn = multiline, TextWrapping = TextWrapping.Wrap, VerticalScrollBarVisibility = ScrollBarVisibility.Auto }; panel.Children.Add(box);
        var ok = new Button { Content = "确定", IsDefault = true, HorizontalAlignment = HorizontalAlignment.Right, Margin = new Thickness(0, 14, 0, 0) }; ok.Click += (_, _) => dialog.DialogResult = true; panel.Children.Add(ok); dialog.Content = panel;
        return dialog.ShowDialog() == true ? box.Text : null;
    }
    public static void Open(string path) => Process.Start(new ProcessStartInfo(path) { UseShellExecute = true });
    public static StackPanel Field(string label, UIElement input) { var p = new StackPanel { Margin = new Thickness(0, 0, 0, 14) }; p.Children.Add(Text(label, 13)); p.Children.Add(input); return p; }
    public static string DataDirectory(string role, string[] args)
    {
        int index = Array.IndexOf(args, "--data-dir");
        return index >= 0 && index + 1 < args.Length ? Path.GetFullPath(args[index + 1]) : LocalFiles.DataRoot(role);
    }
}

internal sealed class Tray : IDisposable
{
    readonly Forms.NotifyIcon icon;
    bool exiting;
    public Tray(Window window, Func<Task> shutdown)
    {
        icon = new Forms.NotifyIcon { Icon = System.Drawing.SystemIcons.Application, Text = window.Title.Length > 63 ? window.Title[..63] : window.Title, Visible = true };
        var menu = new Forms.ContextMenuStrip(); menu.Items.Add("打开", null, (_, _) => Show());
        menu.Items.Add("退出", null, async (_, _) => { exiting = true; icon.Visible = false; await shutdown(); System.Windows.Application.Current.Shutdown(); });
        icon.ContextMenuStrip = menu; icon.DoubleClick += (_, _) => Show();
        window.Closing += (_, e) => { if (!exiting) { e.Cancel = true; window.Hide(); } };
        void Show() { window.Show(); window.WindowState = WindowState.Normal; window.Activate(); }
    }
    public void Notify(string message) { icon.BalloonTipTitle = "Codex 部门日报"; icon.BalloonTipText = message; icon.ShowBalloonTip(5000); }
    public void Dispose() => icon.Dispose();
}

internal sealed class StatusBrushConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, CultureInfo culture)
    {
        string text = value as string ?? "";
        return new SolidColorBrush(text.StartsWith("🟢") ? Color.FromRgb(28, 139, 88) : text.StartsWith("🟡") ? Color.FromRgb(205, 140, 22) : Color.FromRgb(194, 57, 63));
    }
    public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture) => throw new NotSupportedException();
}
internal sealed class StatusTextConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, CultureInfo culture) => (value as string ?? "").Replace("🟢", "").Replace("🟡", "").Replace("🔴", "").Trim();
    public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture) => throw new NotSupportedException();
}
