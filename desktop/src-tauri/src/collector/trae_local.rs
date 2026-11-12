use super::{local_day_start, truncate, LABEL_TRAE_LOCAL};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::{types::ValueRef, Connection, OpenFlags};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};
use walkdir::WalkDir;

const TOOL: &str = "traeLocal";
/// One state.vscdb above this size is copied to a temp file only up to this limit.
const MAX_DB_BYTES: u64 = 200 * 1024 * 1024;
/// Bound the scan so a very large workspace history cannot stall a collection.
const MAX_FILES: usize = 200;
/// Session messages older/newer than the target day are filtered individually.
const MAX_SESSION_TITLE: usize = 100;

const CHAT_KEYS: [&str; 2] = ["memento/icube-ai-agent-storage", "chat.ChatSessionStore.index"];
const CHAT_KEY_PREFIXES: [&str; 4] = [
    "memento/icube-ai-ng-chat-storage",
    "icube-ai-agent-storage",
    "icube-ai-chat-storage",
    "memento/icube-ai-chat-storage",
];

/// The international Trae builds keep chat data in plaintext VS Code-style
/// `state.vscdb` files under the user's workspace storage. Trae CN uses an encrypted
/// `database.db` instead and is intentionally not read here (export import covers it).
pub(crate) fn default_root() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("Trae")
        .join("User")
        .join("workspaceStorage")
    }
    #[cfg(not(windows))]
    {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
            })
            .unwrap_or_default();
        config.join("Trae").join("User").join("workspaceStorage")
    }
}

/// Locate the newest readable plaintext state.vscdb files. Encrypted or malformed files
/// are reported through warnings and never abort the collection.
pub(crate) fn collect(
    root: &Path,
    target_day: NaiveDate,
    timezone: Tz,
    cutoff: DateTime<Utc>,
) -> Result<super::SourceCapture> {
    let mut global_warnings = Vec::new();
    if !root.is_dir() {
        global_warnings.push(format!("{LABEL_TRAE_LOCAL}：目录不存在或不可读"));
        return Ok(super::SourceCapture {
            sessions: Vec::new(),
            events: Vec::new(),
            global_warnings,
        });
    }
    let day_start = local_day_start(target_day, timezone).unwrap_or_else(Utc::now);
    let mut files = Vec::new();
    for entry in WalkDir::new(root).max_depth(3).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                global_warnings.push(format!(
                    "{LABEL_TRAE_LOCAL}：无法扫描部分工作区目录，采集不完整"
                ));
                continue;
            }
        };
        if !entry.file_type().is_file()
            || entry.file_name().to_string_lossy() != "state.vscdb"
        {
            continue;
        }
        let path = entry.path().to_owned();
        let activity = file_activity(&path);
        // A file that has not been written since before the target day cannot contain
        // visible records for that day; skip it before copying/opening.
        if activity.is_some_and(|activity| activity + chrono::Duration::hours(1) < day_start) {
            continue;
        }
        files.push(path);
        if files.len() >= MAX_FILES {
            global_warnings.push(format!(
                "{LABEL_TRAE_LOCAL}：工作区文件超过 {MAX_FILES} 个，仅采集最近修改的文件"
            ));
            break;
        }
    }
    if files.is_empty() {
        return Ok(super::SourceCapture {
            sessions: Vec::new(),
            events: Vec::new(),
            global_warnings,
        });
    }
    files.sort_by_cached_key(|path| std::cmp::Reverse(file_activity(path)));

    let work = std::env::temp_dir().join(format!(
        "daily-report-trae-local-{}",
        crate::common::id()
    ));
    fs::create_dir_all(&work).context("无法创建 Trae 本地记录临时目录")?;
    let mut sessions = Vec::new();
    let mut events = Vec::new();
    let mut unreadable = 0usize;
    let mut encrypted = 0usize;
    let mut untimed = 0usize;
    for path in files {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        let copy = match copy_snapshot(&path, &work) {
            Ok(copy) => copy,
            Err(_) => {
                unreadable += 1;
                global_warnings.push(format!(
                    "{LABEL_TRAE_LOCAL}：无法读取 {relative}，采集不完整"
                ));
                continue;
            }
        };
        if !is_plaintext_sqlite(&copy) {
            encrypted += 1;
            let _ = fs::remove_file(&copy);
            continue;
        }
        let connection = match Connection::open_with_flags(
            &copy,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(connection) => connection,
            Err(_) => {
                unreadable += 1;
                let _ = fs::remove_file(&copy);
                global_warnings.push(format!(
                    "{LABEL_TRAE_LOCAL}：{relative} 不是可读的明文 SQLite 库，未采集"
                ));
                continue;
            }
        };
        let raw_sessions = match read_chat_sessions(&connection) {
            Ok(raw_sessions) => raw_sessions,
            Err(_) => {
                unreadable += 1;
                global_warnings.push(format!(
                    "{LABEL_TRAE_LOCAL}：{relative} 的会话表无法读取，未采集"
                ));
                let _ = fs::remove_file(&copy);
                continue;
            }
        };
        drop(connection);
        let _ = fs::remove_file(&copy);
        for (index, raw) in raw_sessions.iter().enumerate() {
            let session_id = raw.session_id.clone().unwrap_or_else(|| {
                format!(
                    "local:{}:{}",
                    &crate::common::hash(&relative)[..12],
                    index + 1
                )
            });
            let id = format!("{TOOL}:{}", &crate::common::hash(&session_id)[..16]);
            let mut local = Vec::new();
            let mut used_fallback = false;
            for (ordinal, message) in raw.messages.iter().enumerate() {
                let at = match message.at {
                    Some(value) => {
                        if value.with_timezone(&timezone).date_naive() != target_day {
                            continue;
                        }
                        value
                    }
                    None => {
                        // Without a per-message timestamp the workspace file date is the
                        // only signal; it is reported as an informational warning.
                        let fallback = file_activity(&path).unwrap_or_else(Utc::now);
                        if fallback.with_timezone(&timezone).date_naive() != target_day {
                            continue;
                        }
                        used_fallback = true;
                        fallback
                    }
                };
                if at > cutoff {
                    continue;
                }
                if message.text.trim().is_empty() {
                    continue;
                }
                let stamp = at
                    .with_timezone(&timezone)
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
                let evidence_id = format!(
                    "E{}",
                    &crate::common::hash(&format!("{id}:{}:{stamp}", ordinal + 1))[..20]
                );
                local.push(serde_json::json!({"id":evidence_id,"sessionId":id,
                    "timestamp":stamp,"kind":message.role,
                    "text":truncate(crate::common::redact(&message.text)),
                    "source":format!("{relative}:{}",ordinal + 1),"tool":TOOL}));
            }
            if local.is_empty() {
                continue;
            }
            if used_fallback {
                untimed += 1;
            }
            let title = raw
                .title
                .clone()
                .filter(|title| !title.trim().is_empty())
                .map(|title| {
                    crate::common::redact(title.trim())
                        .chars()
                        .take(MAX_SESSION_TITLE)
                        .collect::<String>()
                })
                .unwrap_or_else(|| "Trae 本地会话".to_owned());
            sessions.push(serde_json::json!({
                "id":id,"title":title,"project":"","warnings":[],
                "tool":TOOL,"toolLabel":LABEL_TRAE_LOCAL,
                "firstAt":local.first().map(|event| event["timestamp"].clone()).unwrap_or(Value::Null),
                "lastAt":local.last().map(|event| event["timestamp"].clone()).unwrap_or(Value::Null),
                "eventCount":local.len()}));
            events.extend(local);
        }
    }
    let _ = fs::remove_dir_all(&work);
    if encrypted > 0 {
        global_warnings.push(format!(
            "{LABEL_TRAE_LOCAL}：{encrypted} 个工作区数据库不是明文 SQLite（可能是 Trae CN 加密库），请改用 Trae 导出导入"
        ));
    }
    if unreadable > 0 {
        global_warnings.push(format!(
            "{LABEL_TRAE_LOCAL}：{unreadable} 个工作区数据库无法解析，未采集"
        ));
    }
    if untimed > 0 {
        global_warnings.push(format!(
            "说明：{LABEL_TRAE_LOCAL}：{untimed} 个本地会话缺少逐条时间戳，按文件日期计入"
        ));
    }
    Ok(super::SourceCapture {
        sessions,
        events,
        global_warnings,
    })
}

fn file_activity(path: &Path) -> Option<DateTime<Utc>> {
    let mut newest = None;
    for candidate in [path.to_owned(), PathBuf::from(format!("{}-wal", path.display()))] {
        if let Ok(modified) = fs::metadata(&candidate).and_then(|metadata| metadata.modified()) {
            let modified: SystemTime = modified;
            let modified: DateTime<Utc> = modified.into();
            newest = Some(newest.map_or(modified, |current: DateTime<Utc>| current.max(modified)));
        }
    }
    newest
}

/// Copy the database and its WAL/SHM siblings so a read-only player sees a consistent
/// snapshot even while Trae keeps the original open (WAL recovery needs write access).
fn copy_snapshot(source: &Path, work: &Path) -> Result<PathBuf> {
    let name = format!(
        "{}-{}.vscdb",
        &crate::common::hash(&source.to_string_lossy())[..16],
        crate::common::id()
    );
    let target = work.join(&name);
    let size = fs::metadata(source)?.len();
    if size > MAX_DB_BYTES {
        anyhow::bail!("database too large");
    }
    fs::copy(source, &target)?;
    for suffix in ["-wal", "-shm"] {
        let companion = PathBuf::from(format!("{}{suffix}", source.display()));
        if companion.exists() {
            fs::copy(&companion, PathBuf::from(format!("{}{suffix}", target.display())))?;
        }
    }
    Ok(target)
}

fn is_plaintext_sqlite(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut header = [0u8; 16];
    file.read_exact(&mut header).is_ok() && header == *b"SQLite format 3\0"
}

struct RawSession {
    session_id: Option<String>,
    title: Option<String>,
    messages: Vec<RawMessage>,
}

struct RawMessage {
    role: String,
    text: String,
    at: Option<DateTime<Utc>>,
}

fn read_chat_sessions(connection: &Connection) -> Result<Vec<RawSession>> {
    let mut statement = connection.prepare("SELECT key, value FROM ItemTable")?;
    let rows = statement.query_map([], |row| {
        let key: String = row.get(0)?;
        let value = match row.get_ref(1)? {
            ValueRef::Text(bytes) | ValueRef::Blob(bytes) => Some(bytes.to_vec()),
            _ => None,
        };
        Ok((key, value))
    })?;
    let mut sessions = Vec::new();
    for row in rows {
        let (key, value) = row?;
        if !is_chat_key(&key) {
            continue;
        }
        let Some(bytes) = value else { continue };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        collect_session_values(&json, &mut sessions, 0);
    }
    Ok(sessions)
}

fn is_chat_key(key: &str) -> bool {
    CHAT_KEYS.contains(&key) || CHAT_KEY_PREFIXES.iter().any(|prefix| key.starts_with(prefix))
}

fn collect_session_values(value: &Value, sessions: &mut Vec<RawSession>, depth: usize) {
    if depth > 8 {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                collect_session_values(item, sessions, depth + 1);
            }
        }
        Value::Object(object) => {
            if let Some(messages) = object.get("messages").and_then(Value::as_array) {
                let session = raw_session_from(object, messages);
                if !session.messages.is_empty() {
                    sessions.push(session);
                }
                return;
            }
            for key in ["list", "value", "data", "sessions", "conversations", "items"] {
                if let Some(child) = object.get(key) {
                    collect_session_values(child, sessions, depth + 1);
                }
            }
        }
        _ => {}
    }
}

fn raw_session_from(
    object: &serde_json::Map<String, Value>,
    messages: &[Value],
) -> RawSession {
    let session_id = ["sessionId", "session_id", "id"]
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .map(str::to_owned);
    let mut parsed = Vec::new();
    for message in messages {
        let Some(object) = message.as_object() else {
            continue;
        };
        if object.get("status").and_then(Value::as_str) == Some("deleted") {
            continue;
        }
        let Some(role) = object.get("role").and_then(Value::as_str) else {
            continue;
        };
        let role = match role {
            "user" => "user",
            "assistant" => "assistant",
            _ => continue,
        };
        let mut parts = Vec::new();
        if let Some(content) = object.get("content").and_then(Value::as_str) {
            if !content.trim().is_empty() {
                parts.push(content.trim().to_owned());
            }
        }
        if let Some(queries) = object.get("parsedQuery").and_then(Value::as_array) {
            for query in queries.iter().filter_map(Value::as_str) {
                let query = query.trim();
                if !query.is_empty() {
                    parts.push(query.to_owned());
                }
            }
        }
        let text = parts.join("\n");
        if text.is_empty() {
            continue;
        }
        parsed.push(RawMessage {
            role: role.to_owned(),
            text,
            at: message_timestamp(object),
        });
    }
    let title = ["title", "sessionTitle", "session_title", "name"]
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
        .or_else(|| {
            parsed
                .iter()
                .find(|message| message.role == "user")
                .map(|message| message.text.chars().take(MAX_SESSION_TITLE).collect())
        });
    RawSession {
        session_id,
        title,
        messages: parsed,
    }
}

fn message_timestamp(object: &serde_json::Map<String, Value>) -> Option<DateTime<Utc>> {
    for key in [
        "timestamp", "time", "createdAt", "created_at", "sendTime", "ts", "date",
    ] {
        if let Some(value) = object.get(key) {
            if let Some(at) = value_as_time(value) {
                return Some(at);
            }
        }
    }
    None
}

fn value_as_time(value: &Value) -> Option<DateTime<Utc>> {
    if let Some(number) = value.as_i64() {
        let millis = if number.abs() > 1_000_000_000_000 {
            number
        } else {
            number * 1000
        };
        return DateTime::<Utc>::from_timestamp_millis(millis);
    }
    let text = value.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(text) {
        return Some(parsed.with_timezone(&Utc));
    }
    if let Ok(parsed) = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.fZ") {
        return Some(Utc.from_utc_datetime(&parsed));
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(parsed) = NaiveDateTime::parse_from_str(text, format) {
            return Some(Utc.from_utc_datetime(&parsed));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::{collect_for_selection, SourceRoots};

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "daily-report-trae-local-{tag}-{}",
                crate::common::id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }
        fn workspace(&self, name: &str) -> PathBuf {
            let path = self.root.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path
        }
        fn day(&self) -> String {
            crate::common::today("Asia/Shanghai").unwrap()
        }
        fn collect(&self) -> crate::collector::SelectionCapture {
            collect_for_selection(
                &SourceRoots {
                    codex: None,
                    dsh: None,
                    trae: None,
                    trae_local: Some(self.root.clone()),
                },
                &self.day(),
                "Asia/Shanghai",
            )
            .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn write_vscdb(path: &Path, items: &[(&str, String)]) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)",
                [],
            )
            .unwrap();
        for (key, value) in items {
            connection
                .execute("INSERT INTO ItemTable (key, value) VALUES (?1, ?2)", rusqlite::params![key, value])
                .unwrap();
        }
        connection.close().unwrap();
    }

    fn now_ms() -> i64 {
        Utc::now().timestamp_millis()
    }

    #[test]
    fn trae_local_reads_plaintext_workspace_state() {
        let fixture = Fixture::new("plaintext");
        let workspace = fixture.workspace("ws-1");
        let payload = serde_json::json!({"list": [
            {"sessionId": "session-a", "title": "本地会话 A", "messages": [
                {"role": "user", "content": "LOCAL_USER_VISIBLE 排查构建问题", "timestamp": now_ms() - 60_000},
                {"role": "assistant", "content": "LOCAL_ASSISTANT_VISIBLE 已修复", "timestamp": now_ms() - 30_000},
                {"role": "assistant", "status": "deleted", "content": "LOCAL_DELETED_MUST_NOT_APPEAR", "timestamp": now_ms()}
            ]}
        ]});
        write_vscdb(
            &workspace.join("state.vscdb"),
            &[("memento/icube-ai-agent-storage", payload.to_string())],
        );
        let result = fixture.collect();
        let capture = &result.capture;
        let events = capture["events"].as_array().unwrap();
        assert_eq!(events.len(), 2, "events: {events:?}");
        assert_eq!(events[0]["kind"], "user");
        assert_eq!(events[1]["kind"], "assistant");
        assert!(events.iter().all(|event| event["tool"] == "traeLocal"));
        assert!(!capture.to_string().contains("LOCAL_DELETED_MUST_NOT_APPEAR"));
        assert_eq!(result.sessions[0]["title"], "本地会话 A");
        assert_eq!(result.sessions[0]["toolLabel"], LABEL_TRAE_LOCAL);
    }

    #[test]
    fn trae_local_skips_messages_from_other_days() {
        let fixture = Fixture::new("other-day");
        let workspace = fixture.workspace("ws-old");
        let yesterday = (Utc::now() - chrono::Duration::days(3)).timestamp_millis();
        let payload = serde_json::json!({"list": [
            {"sessionId": "old", "messages": [
                {"role": "user", "content": "OLD_DAY_MUST_NOT_APPEAR", "timestamp": yesterday}
            ]}
        ]});
        write_vscdb(
            &workspace.join("state.vscdb"),
            &[("memento/icube-ai-agent-storage", payload.to_string())],
        );
        let capture = fixture.collect().capture;
        assert!(capture["events"].as_array().unwrap().is_empty());
    }

    #[test]
    fn trae_local_encrypted_database_is_reported_and_skipped() {
        let fixture = Fixture::new("encrypted");
        let workspace = fixture.workspace("ws-encrypted");
        std::fs::write(workspace.join("state.vscdb"), [0x11u8; 4096]).unwrap();
        let capture = fixture.collect().capture;
        assert!(capture["events"].as_array().unwrap().is_empty());
        assert!(capture["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap()
                .contains("请改用 Trae 导出导入")));
    }

    #[test]
    fn trae_local_invalid_chat_payload_is_ignored() {
        let fixture = Fixture::new("invalid");
        let workspace = fixture.workspace("ws-invalid");
        write_vscdb(
            &workspace.join("state.vscdb"),
            &[("memento/icube-ai-agent-storage", "not-json".to_owned())],
        );
        let capture = fixture.collect().capture;
        assert!(capture["events"].as_array().unwrap().is_empty());
    }

    #[test]
    fn trae_local_redacts_secrets_in_visible_text() {
        let fixture = Fixture::new("redact");
        let workspace = fixture.workspace("ws-redact");
        let payload = serde_json::json!({"list": [
            {"sessionId": "redact", "messages": [
                {"role": "user", "content": "密钥 sk-abcdefghijklmnopqrst 需要轮换", "timestamp": now_ms()}
            ]}
        ]});
        write_vscdb(
            &workspace.join("state.vscdb"),
            &[("memento/icube-ai-agent-storage", payload.to_string())],
        );
        let capture = fixture.collect().capture;
        let serialized = capture.to_string();
        assert!(!serialized.contains("sk-abcdefghijklmnopqrst"));
        assert!(serialized.contains("需要轮换"));
    }

    #[test]
    fn trae_local_reads_a_live_wal_database() {
        // Trae uses the VS Code SQLite stack; when it is running, recent rows can live
        // only in the -wal file. The reader copies db+wal+shm before opening, so a live
        // snapshot must still contain those records.
        let fixture = Fixture::new("wal-live");
        let workspace = fixture.workspace("ws-wal");
        let database = workspace.join("state.vscdb");
        let connection = Connection::open(&database).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        connection
            .execute(
                "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)",
                [],
            )
            .unwrap();
        let payload = serde_json::json!({"list": [
            {"sessionId": "wal-session", "title": "WAL 会话", "messages": [
                {"role": "user", "content": "WAL_USER_VISIBLE 实时数据库", "timestamp": now_ms() - 30_000},
                {"role": "assistant", "content": "WAL_ASSISTANT_VISIBLE 已完成", "timestamp": now_ms() - 20_000}
            ]}
        ]});
        connection
            .execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                rusqlite::params!["memento/icube-ai-agent-storage", payload.to_string()],
            )
            .unwrap();
        assert!(workspace.join("state.vscdb-wal").exists(), "WAL file missing");
        // Keep the writer connection open: this mirrors collecting while Trae runs.
        let result = fixture.collect();
        assert_eq!(
            result.capture["events"].as_array().unwrap().len(),
            2,
            "live WAL snapshot lost records: {}",
            result.capture
        );
        assert_eq!(result.sessions[0]["title"], "WAL 会话");
        assert_eq!(result.sessions[0]["tool"], "traeLocal");
        connection.close().unwrap();
    }
}
