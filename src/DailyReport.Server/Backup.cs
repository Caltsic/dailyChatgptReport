using System.IO.Compression;
using DailyReport.Core;
using Microsoft.Data.Sqlite;

namespace DailyReport.Server;

public static class Backup
{
    public static string Create(Store store, string directory)
    {
        Directory.CreateDirectory(directory);
        var staging = Path.Combine(directory, ".stage-" + Wire.Id()); Directory.CreateDirectory(staging);
        string departmentId = store.Read(r => r.Get<DepartmentSettings>("settings"))?.Id ?? throw new InvalidOperationException("尚未初始化部门");
        string prefix = "department-" + Wire.Hash(departmentId)[..16] + "-";
        string destination = Path.Combine(directory, prefix + DateTime.Now.ToString("yyyyMMdd-HHmmss") + "-" + Wire.Id()[..6] + ".zip");
        try
        {
            lock (store.Gate)
            {
                using (var source = store.Open()) using (var target = new SqliteConnection(new SqliteConnectionStringBuilder { DataSource = Path.Combine(staging, "department.db"), Pooling = false }.ToString())) { target.Open(); source.BackupDatabase(target); }
                foreach (string folder in new[] { "submissions", "reports" })
                {
                    Directory.CreateDirectory(Path.Combine(staging, folder));
                    foreach (var file in Directory.GetFiles(Path.Combine(store.Root, folder))) if (!file.EndsWith(".tmp")) File.Copy(file, Path.Combine(staging, folder, Path.GetFileName(file)));
                }
                // Exclude device credentials from portable backups. They must be re-paired on restore.
                var copy = new Store(staging); copy.Write(r => { foreach (var d in r.All<Device>()) r.Delete<Device>(d.Id); foreach (var i in r.All<InviteRecord>()) r.Delete<InviteRecord>(i.Id); foreach (var j in r.All<Job>().Where(j => j.State is "running" or "queued")) r.Put(j.Id, j with { State = "failed", Error = "从备份恢复，请配置模型后重新生成" }, j.Day); });
                File.WriteAllText(Path.Combine(staging, "backup.json"), Wire.Serialize(new { format = 1, createdAt = DateTimeOffset.UtcNow, department = store.Read(r => r.Get<DepartmentSettings>("settings"))?.Name }));
                ZipFile.CreateFromDirectory(staging, destination + ".tmp"); File.Move(destination + ".tmp", destination);
            }
            foreach (var old in Directory.GetFiles(directory, prefix + "*.zip").OrderByDescending(File.GetLastWriteTimeUtc).Skip(14)) File.Delete(old);
            return destination;
        }
        finally { Directory.Delete(staging, true); if (File.Exists(destination + ".tmp")) File.Delete(destination + ".tmp"); }
    }
    public static string? Daily(Store store)
    {
        var settings = store.Read(r => r.Get<DepartmentSettings>("settings")); if (settings == null) return null;
        string marker = Path.Combine(store.Root, "last-backup.txt"), day = Wire.Today(settings.TimeZone).ToString("yyyy-MM-dd");
        if (File.Exists(marker) && File.ReadAllText(marker) == day) return null;
        var result = Create(store, Path.Combine(store.Root, "backups")); LocalFiles.AtomicText(marker, day); return result;
    }
    public static void Restore(string archive, string destination)
    {
        destination = Path.GetFullPath(destination);
        if (Directory.Exists(destination) && Directory.EnumerateFileSystemEntries(destination).Any()) throw new InvalidOperationException("为保留现有数据，恢复目标必须是空文件夹");
        var staging = destination + ".restore-" + Wire.Id(); Directory.CreateDirectory(staging);
        try
        {
            using (var zip = ZipFile.OpenRead(archive))
            {
                long expanded = 0;
                foreach (var entry in zip.Entries)
                {
                    string name = entry.FullName.Replace('\\', '/');
                    if (name.EndsWith('/')) continue;
                    if (!(name is "department.db" or "backup.json" || name.StartsWith("submissions/") || name.StartsWith("reports/"))) throw new InvalidDataException("备份包含不支持的文件");
                    string path = Path.GetFullPath(Path.Combine(staging, name));
                    if (!path.StartsWith(staging + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase)) throw new InvalidDataException("备份路径无效");
                    expanded += entry.Length; if (expanded > 20L * 1024 * 1024 * 1024) throw new InvalidDataException("备份解压体积过大");
                    Directory.CreateDirectory(Path.GetDirectoryName(path)!); entry.ExtractToFile(path, false);
                }
            }
            if (!File.Exists(Path.Combine(staging, "backup.json")) || !File.Exists(Path.Combine(staging, "department.db"))) throw new InvalidDataException("不是有效的部门备份");
            var restored = new Store(staging);
            if (restored.Read(r => r.Get<DepartmentSettings>("settings")) == null) throw new InvalidDataException("备份缺少部门设置");
            using (var db = restored.Open()) using (var cmd = db.CreateCommand()) { cmd.CommandText = "PRAGMA integrity_check"; if ((string?)cmd.ExecuteScalar() != "ok") throw new InvalidDataException("备份数据库完整性校验失败"); }
            restored.Write(r => { foreach (var d in r.All<Device>()) r.Delete<Device>(d.Id); foreach (var i in r.All<InviteRecord>()) r.Delete<InviteRecord>(i.Id); });
            var service = new DepartmentService(restored);
            foreach (var snapshot in restored.Read(r => r.All<Snapshot>())) _ = service.LoadCapture(snapshot);
            if (Directory.Exists(destination)) Directory.Delete(destination); // Empty target verified above; never recursively delete user data.
            Directory.Move(staging, destination);
        }
        finally { if (Directory.Exists(staging)) Directory.Delete(staging, true); }
    }
}
