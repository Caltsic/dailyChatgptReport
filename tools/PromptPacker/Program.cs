using System.Security.Cryptography;
using System.Text;
// Obfuscation at rest only: the trusted leader application must be able to decrypt.
var key = SHA256.HashData(Encoding.UTF8.GetBytes("CodexDailyReport:leader-resource:v1:2026"));
Directory.CreateDirectory(args[1]);
foreach (var path in Directory.GetFiles(args[0], "*.txt"))
{
    var plain = Encoding.UTF8.GetBytes(File.ReadAllText(path));
    byte[] nonce = RandomNumberGenerator.GetBytes(12), tag = new byte[16], cipher = new byte[plain.Length];
    using var aes = new AesGcm(key, 16); aes.Encrypt(nonce, plain, cipher, tag);
    File.WriteAllBytes(Path.Combine(args[1], Path.GetFileNameWithoutExtension(path) + ".enc"), nonce.Concat(tag).Concat(cipher).ToArray());
}
