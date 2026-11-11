using Microsoft.Data.Sqlite;
using DailyReport.Core;

namespace DailyReport.Server;

// Every mutation runs in one transaction. A process-wide gate also coordinates backup and files.
public sealed class Store
{
    public string Root { get; }
    internal object Gate { get; } = new();
    readonly string connectionString;
    public Store(string root)
    {
        Root = Path.GetFullPath(root); Directory.CreateDirectory(Root);
        Directory.CreateDirectory(Path.Combine(Root, "submissions")); Directory.CreateDirectory(Path.Combine(Root, "reports"));
        connectionString = new SqliteConnectionStringBuilder { DataSource = Path.Combine(Root, "department.db"), Pooling = false }.ToString();
        using var conn = Open(); using var cmd = conn.CreateCommand();
        cmd.CommandText = "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS entities (kind TEXT NOT NULL,id TEXT NOT NULL,day TEXT NOT NULL,json TEXT NOT NULL,PRIMARY KEY(kind,id)); CREATE INDEX IF NOT EXISTS ix_entities_day ON entities(kind,day);"; cmd.ExecuteNonQuery();
    }
    internal SqliteConnection Open() { var c = new SqliteConnection(connectionString); c.Open(); return c; }
    public T Read<T>(Func<Rows, T> action) { lock (Gate) { using var c = Open(); return action(new Rows(c, null)); } }
    public T Write<T>(Func<Rows, T> action) { lock (Gate) { using var c = Open(); using var tx = c.BeginTransaction(); T result = action(new Rows(c, tx)); tx.Commit(); return result; } }
    public void Write(Action<Rows> action) => Write(r => { action(r); return true; });
    public sealed class Rows(SqliteConnection conn, SqliteTransaction? tx)
    {
        SqliteCommand Command(string sql, params (string, object)[] values)
        {
            var cmd = conn.CreateCommand(); cmd.Transaction = tx; cmd.CommandText = sql;
            foreach (var (key, value) in values) cmd.Parameters.AddWithValue(key, value); return cmd;
        }
        public T? Get<T>(string id) where T : class
        {
            using var cmd = Command("SELECT json FROM entities WHERE kind=$k AND id=$i", ("$k", typeof(T).Name), ("$i", id));
            return cmd.ExecuteScalar() is string json ? Wire.Parse<T>(json) : null;
        }
        public List<T> All<T>(string? day = null)
        {
            using var cmd = day == null ? Command("SELECT json FROM entities WHERE kind=$k ORDER BY rowid", ("$k", typeof(T).Name)) : Command("SELECT json FROM entities WHERE kind=$k AND day=$d ORDER BY rowid", ("$k", typeof(T).Name), ("$d", day));
            using var reader = cmd.ExecuteReader(); List<T> result = []; while (reader.Read()) result.Add(Wire.Parse<T>(reader.GetString(0))); return result;
        }
        public void Put<T>(string id, T entity, string day = "")
        {
            using var cmd = Command("INSERT INTO entities(kind,id,day,json) VALUES($k,$i,$d,$j) ON CONFLICT(kind,id) DO UPDATE SET day=$d,json=$j", ("$k", typeof(T).Name), ("$i", id), ("$d", day), ("$j", Wire.Serialize(entity))); cmd.ExecuteNonQuery();
        }
        public void Delete<T>(string id) { using var cmd = Command("DELETE FROM entities WHERE kind=$k AND id=$i", ("$k", typeof(T).Name), ("$i", id)); cmd.ExecuteNonQuery(); }
    }
}
