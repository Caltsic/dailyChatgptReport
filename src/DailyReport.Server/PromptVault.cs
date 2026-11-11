using System.Reflection;
using System.Security.Cryptography;
using System.Text;

namespace DailyReport.Server;

internal static class PromptVault
{
    public static string Read(string name)
    {
        var assembly = Assembly.GetExecutingAssembly();
        using var stream = assembly.GetManifestResourceStream("DailyReport.Server.Prompts." + name + ".enc") ?? throw new InvalidOperationException("缺少内置提示词资源");
        using var buffer = new MemoryStream(); stream.CopyTo(buffer); var bytes = buffer.ToArray();
        var plain = new byte[bytes.Length - 28];
        using var aes = new AesGcm(SHA256.HashData(Encoding.UTF8.GetBytes("CodexDailyReport:leader-resource:v1:2026")), 16);
        aes.Decrypt(bytes.AsSpan(0, 12), bytes.AsSpan(28), bytes.AsSpan(12, 16), plain);
        return Encoding.UTF8.GetString(plain);
    }
}
