using System.Security.Cryptography;
using System.Text;

namespace DailyReport.Core;

public static class LocalFiles
{
    public static void AtomicText(string path, string text)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
        string temp = path + "." + Wire.Id() + ".tmp";
        try { File.WriteAllText(temp, text, new UTF8Encoding(false)); File.Move(temp, path, true); }
        finally { if (File.Exists(temp)) File.Delete(temp); }
    }
    public static void SaveSecret(string path, string text) => AtomicText(path, Convert.ToBase64String(ProtectedData.Protect(Encoding.UTF8.GetBytes(text), null, DataProtectionScope.CurrentUser)));
    public static string ReadSecret(string path) => Encoding.UTF8.GetString(ProtectedData.Unprotect(Convert.FromBase64String(File.ReadAllText(path)), null, DataProtectionScope.CurrentUser));
    public static string DataRoot(string role) => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "CodexDailyReport", role);
}
