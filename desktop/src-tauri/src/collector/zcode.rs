use super::{local_day_start, text, truncate, LABEL_ZCODE};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};
use walkdir::WalkDir;

const TOOL: &str = "zcode";
const MAX_DB_BYTES: u64 = 200 * 1024 * 1024;
const MAX_SESSION_TITLE: usize = 100;

struct CollectContext {
    target_day: NaiveDate,
    timezone: Tz,
    cutoff: DateTime<Utc>,
    events: Vec<Value>,
    seen_event_ids: HashSet<String>,
    session_info: HashMap<String, Value>,
    sqlite_session_ids: HashSet<String>,
    global_warnings: Vec<String>,
}

struct TaskMeta {
    workspace_path: String,
    title: String,
    forked_from: String,
    deleted: bool,
}

struct SessionRow {
    id: String,
    parent_id: String,
    directory: String,
    path: String,
    title: String,
}

struct MessageRow {
    id: String,
    session_id: String,
    time_created: i64,
    data: String,
}

struct PartRow {
    message_id: String,
    time_created: i64,
    data: String,
}

struct RawRecord {
    order_key: String,
    utc_ts: DateTime<Utc>,
    kind: String,
    body: String,
    source: String,
    extra: Option<serde_json::Map<String, Value>>,
}

pub(crate) fn default_root() -> PathBuf {
    std::env::var_os("ZCODE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| super::home().join(".zcode"))
}

fn is_plaintext_sqlite(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut header = [0u8; 16];
    file.read_exact(&mut header).is_ok() && header == *b"SQLite format 3\0"
}

fn file_activity(path: &Path) -> Option<DateTime<Utc>> {
    let mut newest = None;
    for candidate in [path.to_owned(), PathBuf::from(format!("{}-wal", path.display()))] {
        if let Ok(modified) = fs::metadata(&candidate).and_then(|meta| meta.modified()) {
            let modified: SystemTime = modified;
            let modified: DateTime<Utc> = modified.into();
            newest = Some(newest.map_or(modified, |cur: DateTime<Utc>| cur.max(modified)));
        }
    }
    newest
}

fn copy_db_snapshot(source: &Path, work: &Path, prefix: &str) -> Result<PathBuf> {
    let size = fs::metadata(source)?.len();
    if size > MAX_DB_BYTES {
        anyhow::bail!("database too large");
    }
    let target = work.join(format!("{prefix}-{}.sqlite", crate::common::id()));
    fs::copy(source, &target)?;
    for suffix in ["-wal", "-shm"] {
        let companion = PathBuf::from(format!("{}{suffix}", source.display()));
        if companion.exists() {
            let _ = fs::copy(
                &companion,
                PathBuf::from(format!("{}{suffix}", target.display())),
            );
        }
    }
    Ok(target)
}

fn parse_ts_value(value: Option<&Value>) -> Option<DateTime<Utc>> {
    match value? {
        Value::Number(num) => parse_epoch_num(num.as_i64()?),
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return None;
            }
            if let Ok(parsed) = DateTime::parse_from_rfc3339(trimmed) {
                return Some(parsed.with_timezone(&Utc));
            }
            if let Ok(n) = trimmed.parse::<i64>() {
                return parse_epoch_num(n);
            }
            None
        }
        _ => None,
    }
}

fn parse_epoch_num(raw: i64) -> Option<DateTime<Utc>> {
    if raw <= 0 {
        return None;
    }
    if raw >= 1_000_000_000_000 {
        DateTime::from_timestamp_millis(raw)
    } else {
        DateTime::from_timestamp(raw, 0)
    }
}

fn json_value_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn read_tasks_index(
    root: &Path,
    work: &Path,
    global_warnings: &mut Vec<String>,
) -> HashMap<String, TaskMeta> {
    let mut map = HashMap::new();
    let idx_path = root.join("v2").join("tasks-index.sqlite");
    if !idx_path.is_file() {
        return map;
    }
    let res = (|| -> Result<HashMap<String, TaskMeta>> {
        let copy = copy_db_snapshot(&idx_path, work, "tasks-index").context("复制任务索引库失败")?;
        if !is_plaintext_sqlite(&copy) {
            anyhow::bail!("tasks-index.sqlite 不是明文 SQLite");
        }
        let conn = Connection::open_with_flags(
            &copy,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let mut out = HashMap::new();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT task_id, workspace_path, title, forked_from_task_id, deleted FROM tasks",
        ) {
            let rows = stmt.query_map([], |row| {
                let id: String = row.get(0)?;
                let ws: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
                let title: String = row.get::<_, Option<String>>(2)?.unwrap_or_default();
                let forked: String = row.get::<_, Option<String>>(3)?.unwrap_or_default();
                let deleted: i64 = row.get::<_, Option<i64>>(4)?.unwrap_or(0);
                Ok((id, ws, title, forked, deleted != 0))
            })?;
            for (id, ws, title, forked, deleted) in rows.flatten() {
                if id.trim().is_empty() {
                    continue;
                }
                out.insert(
                    id,
                    TaskMeta {
                        workspace_path: crate::common::redact(ws.trim()),
                        title: crate::common::redact(title.trim()),
                        forked_from: forked.trim().to_owned(),
                        deleted,
                    },
                );
            }
        }
        Ok(out)
    })();
    match res {
        Ok(loaded) => map = loaded,
        Err(_) => {
            global_warnings.push(format!(
                "{LABEL_ZCODE}：v2/tasks-index.sqlite 无法读取，已仅使用会话主库元数据"
            ));
        }
    }
    map
}

fn db_candidates(root: &Path) -> Option<PathBuf> {
    let nested = root.join("cli").join("db").join("db.sqlite");
    if nested.is_file() {
        return Some(nested);
    }
    let direct = root.join("db.sqlite");
    if direct.is_file() {
        return Some(direct);
    }
    None
}

fn collect_sqlite_store(
    root: &Path,
    db_path: &Path,
    work: &Path,
    tasks_meta: &HashMap<String, TaskMeta>,
    ctx: &mut CollectContext,
) {
    let target_day = ctx.target_day;
    let timezone = ctx.timezone;
    let cutoff = ctx.cutoff;
    let events = &mut ctx.events;
    let seen_event_ids = &mut ctx.seen_event_ids;
    let session_info = &mut ctx.session_info;
    let sqlite_session_ids = &mut ctx.sqlite_session_ids;
    let global_warnings = &mut ctx.global_warnings;
    let rel_db = crate::common::redact(
        &db_path
            .strip_prefix(root)
            .unwrap_or(db_path)
            .to_string_lossy(),
    );
    if let Some(day_start) = local_day_start(target_day, timezone) {
        if let Some(modified) = file_activity(db_path) {
            if modified + chrono::Duration::hours(1) < day_start {
                return;
            }
        }
    }
    let copy = match copy_db_snapshot(db_path, work, "db") {
        Ok(copy) => copy,
        Err(_) => {
            global_warnings.push(format!("{LABEL_ZCODE}：无法读取 {rel_db}，采集不完整"));
            return;
        }
    };
    if !is_plaintext_sqlite(&copy) {
        global_warnings.push(format!("{LABEL_ZCODE}：{rel_db} 不是可读的明文 SQLite 库"));
        return;
    }
    let conn = match Connection::open_with_flags(
        &copy,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(conn) => conn,
        Err(_) => {
            global_warnings.push(format!("{LABEL_ZCODE}：无法打开 {rel_db}，未采集"));
            return;
        }
    };

    let mut sessions = Vec::new();
    match conn.prepare("SELECT id, parent_id, directory, path, title FROM session") {
        Ok(mut stmt) => {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok(SessionRow {
                    id: row.get(0)?,
                    parent_id: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    directory: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    path: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    title: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                })
            }) {
                for row in rows.flatten() {
                    sqlite_session_ids.insert(row.id.clone());
                    sessions.push(row);
                }
            }
        }
        Err(_) => {
            global_warnings.push(format!("{LABEL_ZCODE}：{rel_db} 的 session 表无法读取"));
            return;
        }
    }

    let mut messages_by_session = HashMap::<String, Vec<MessageRow>>::new();
    match conn.prepare(
        "SELECT id, session_id, time_created, data FROM message ORDER BY COALESCE(sequence, 0), time_created, id",
    ) {
        Ok(mut stmt) => {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    time_created: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    data: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                })
            }) {
                for msg in rows.flatten() {
                    messages_by_session
                        .entry(msg.session_id.clone())
                        .or_default()
                        .push(msg);
                }
            }
        }
        Err(_) => {
            global_warnings.push(format!("{LABEL_ZCODE}：{rel_db} 的 message 表无法读取"));
            return;
        }
    }

    let mut parts_by_message = HashMap::<String, Vec<PartRow>>::new();
    match conn.prepare(
        "SELECT message_id, time_created, data FROM part ORDER BY COALESCE(sequence, 0), time_created, id",
    ) {
        Ok(mut stmt) => {
            if let Ok(rows) = stmt.query_map([], |row| {
                Ok(PartRow {
                    message_id: row.get(0)?,
                    time_created: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    data: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                })
            }) {
                for part in rows.flatten() {
                    parts_by_message
                        .entry(part.message_id.clone())
                        .or_default()
                        .push(part);
                }
            }
        }
        Err(_) => {
            global_warnings.push(format!("{LABEL_ZCODE}：{rel_db} 的 part 表无法读取"));
            return;
        }
    }

    for sess in sessions {
        let task_meta = tasks_meta.get(&sess.id);
        if task_meta.is_some_and(|m| m.deleted) {
            continue;
        }
        let sid = format!("{TOOL}:{}", sess.id);
        let msgs = messages_by_session
            .get(&sess.id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut raw_records = Vec::<RawRecord>::new();
        let mut malformed = false;

        for (m_idx, msg) in msgs.iter().enumerate() {
            let msg_data: Value = match serde_json::from_str(&msg.data) {
                Ok(Value::Object(map)) => Value::Object(map),
                _ => {
                    malformed = true;
                    continue;
                }
            };
            let role = match text(&msg_data, "role") {
                "user" => "user",
                "assistant" => "assistant",
                _ => continue,
            };
            let msg_ts = parse_ts_value(msg_data.pointer("/time/created"))
                .or_else(|| parse_epoch_num(msg.time_created));

            let parts = parts_by_message
                .get(&msg.id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let mut has_text_part = false;

            for (p_idx, part) in parts.iter().enumerate() {
                let part_data: Value = match serde_json::from_str(&part.data) {
                    Ok(Value::Object(map)) => Value::Object(map),
                    _ => {
                        malformed = true;
                        continue;
                    }
                };
                match text(&part_data, "type") {
                    // Strictly exclude internal reasoning and non-visible bookkeeping parts.
                    "reasoning" | "step-start" | "step-finish" | "snapshot" | "patch" => continue,
                    "text" => {
                        if part_data.get("ignored").and_then(Value::as_bool) == Some(true)
                            || part_data.get("synthetic").and_then(Value::as_bool) == Some(true)
                        {
                            continue;
                        }
                        let body = text(&part_data, "text").trim();
                        if body.is_empty() {
                            continue;
                        }
                        has_text_part = true;
                        let Some(utc_ts) = parse_ts_value(part_data.pointer("/time/start"))
                            .or_else(|| parse_epoch_num(part.time_created))
                            .or(msg_ts)
                        else {
                            malformed = true;
                            continue;
                        };
                        if utc_ts.with_timezone(&timezone).date_naive() != target_day
                            || utc_ts > cutoff
                        {
                            continue;
                        }
                        raw_records.push(RawRecord {
                            order_key: format!("{m_idx:06}:{p_idx:06}:0"),
                            utc_ts,
                            kind: role.to_owned(),
                            body: body.to_owned(),
                            source: format!("{rel_db}:{}:{}", msg.id, p_idx + 1),
                            extra: None,
                        });
                    }
                    "tool" => {
                        let tool_name = text(&part_data, "tool").trim();
                        let state = &part_data["state"];
                        let call_ts = parse_ts_value(state.pointer("/time/start"))
                            .or_else(|| parse_epoch_num(part.time_created))
                            .or(msg_ts);
                        let res_ts = parse_ts_value(state.pointer("/time/end")).or(call_ts);

                        if !tool_name.is_empty() {
                            if let Some(utc_ts) = call_ts {
                                if utc_ts.with_timezone(&timezone).date_naive() == target_day
                                    && utc_ts <= cutoff
                                {
                                    let mut body = tool_name.to_owned();
                                    if let Some(input) = state.get("input").filter(|v| !v.is_null())
                                    {
                                        if let Ok(serialized) = serde_json::to_string(input) {
                                            if !serialized.is_empty() && serialized != "{}" {
                                                body.push('\n');
                                                body.push_str(&serialized);
                                            }
                                        }
                                    }
                                    raw_records.push(RawRecord {
                                        order_key: format!("{m_idx:06}:{p_idx:06}:1"),
                                        utc_ts,
                                        kind: "tool_call".to_owned(),
                                        body,
                                        source: format!("{rel_db}:{}:{}", msg.id, p_idx + 1),
                                        extra: None,
                                    });
                                }
                            } else {
                                malformed = true;
                            }
                        }

                        let status = text(state, "status");
                        let err_msg = json_value_text(state.get("error")).trim().to_owned();
                        let out_msg = json_value_text(state.get("output")).trim().to_owned();
                        let is_error = status == "error" || !err_msg.is_empty();
                        let res_body = if is_error && !err_msg.is_empty() {
                            err_msg
                        } else {
                            out_msg
                        };
                        if !res_body.is_empty() {
                            if let Some(utc_ts) = res_ts {
                                if utc_ts.with_timezone(&timezone).date_naive() == target_day
                                    && utc_ts <= cutoff
                                {
                                    let mut extra = serde_json::Map::new();
                                    if is_error {
                                        extra.insert("error".to_owned(), json!(true));
                                    }
                                    raw_records.push(RawRecord {
                                        order_key: format!("{m_idx:06}:{p_idx:06}:2"),
                                        utc_ts,
                                        kind: "tool_result".to_owned(),
                                        body: res_body,
                                        source: format!("{rel_db}:{}:{}", msg.id, p_idx + 1),
                                        extra: (!extra.is_empty()).then_some(extra),
                                    });
                                }
                            } else {
                                malformed = true;
                            }
                        }
                    }
                    _ => {}
                }
            }

            if !has_text_part {
                let fallback_body = json_value_text(msg_data.get("content")).trim().to_owned();
                if !fallback_body.is_empty() {
                    if let Some(utc_ts) = msg_ts {
                        if utc_ts.with_timezone(&timezone).date_naive() == target_day
                            && utc_ts <= cutoff
                        {
                            raw_records.push(RawRecord {
                                order_key: format!("{m_idx:06}:fallback"),
                                utc_ts,
                                kind: role.to_owned(),
                                body: fallback_body,
                                source: format!("{rel_db}:{}", msg.id),
                                extra: None,
                            });
                        }
                    } else {
                        malformed = true;
                    }
                }
            }
        }

        if raw_records.is_empty() && !malformed {
            continue;
        }

        for rec in raw_records {
            push_record(&sid, rec, timezone, events, seen_event_ids);
        }

        let project = first_nonempty(&[
            crate::common::redact(sess.directory.trim()),
            crate::common::redact(sess.path.trim()),
            task_meta
                .map(|m| m.workspace_path.clone())
                .unwrap_or_default(),
        ]);
        let title = first_nonempty(&[
            crate::common::redact(sess.title.trim()),
            task_meta.map(|m| m.title.clone()).unwrap_or_default(),
        ]);
        let parent_id = first_nonempty(&[
            sess.parent_id.trim().to_owned(),
            task_meta.map(|m| m.forked_from.clone()).unwrap_or_default(),
        ]);
        let mut info = json!({
            "id": sid,
            "title": title,
            "project": project,
            "warnings": [],
            "tool": TOOL,
            "toolLabel": LABEL_ZCODE,
        });
        if !parent_id.is_empty() {
            info["parentSession"] = json!(format!("{TOOL}:{parent_id}"));
            info["delegationDepth"] = json!(1);
        }
        if malformed {
            info["warnings"].as_array_mut().unwrap().push(json!(format!(
                "{rel_db} 中会话 {} 含损坏或无时间戳记录，采集可能不完整",
                sess.id
            )));
        }
        session_info.insert(sid, info);
    }
}

fn collect_legacy_snapshots(
    root: &Path,
    tasks_meta: &HashMap<String, TaskMeta>,
    ctx: &mut CollectContext,
) {
    let target_day = ctx.target_day;
    let timezone = ctx.timezone;
    let cutoff = ctx.cutoff;
    let events = &mut ctx.events;
    let seen_event_ids = &mut ctx.seen_event_ids;
    let session_info = &mut ctx.session_info;
    let sqlite_session_ids = &ctx.sqlite_session_ids;
    let global_warnings = &mut ctx.global_warnings;
    let sessions_dir = root.join("v2").join("sessions");
    if !sessions_dir.is_dir() {
        return;
    }
    let day_start = local_day_start(target_day, timezone);
    let mut files = Vec::new();
    for entry in WalkDir::new(&sessions_dir).max_depth(3).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                global_warnings.push(format!("{LABEL_ZCODE}：无法扫描部分旧版会话目录"));
                continue;
            }
        };
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(|e| e.to_str()) != Some("json")
        {
            continue;
        }
        let path = entry.path().to_owned();
        if let Some(start) = day_start {
            if let Ok(modified) = fs::metadata(&path).and_then(|m| m.modified()) {
                let modified: DateTime<Utc> = modified.into();
                if modified + chrono::Duration::hours(1) < start {
                    continue;
                }
            }
        }
        files.push(path);
    }
    files.sort();

    for path in files {
        let rel = crate::common::redact(
            &path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy(),
        );
        let Ok(raw) = fs::read_to_string(&path) else {
            global_warnings.push(format!("{LABEL_ZCODE}：无法读取旧版快照 {rel}"));
            continue;
        };
        let Ok(doc) = serde_json::from_str::<Value>(raw.trim_start_matches('\u{feff}')) else {
            global_warnings.push(format!("{LABEL_ZCODE}：旧版快照 {rel} 格式无效，未采集"));
            continue;
        };
        let meta = &doc["meta"];
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_owned();
        let raw_id = first_nonempty(&[
            text(meta, "acpSessionId").trim().to_owned(),
            text(meta, "taskId").trim().to_owned(),
            stem,
        ]);
        if raw_id.is_empty() || sqlite_session_ids.contains(&raw_id) {
            continue;
        }
        if tasks_meta.get(&raw_id).is_some_and(|m| m.deleted) {
            continue;
        }

        let sid = format!("{TOOL}:{raw_id}");
        let fallback_ts = parse_ts_value(meta.get("updatedAt"))
            .or_else(|| parse_ts_value(meta.get("createdAt")));
        let messages = doc["messages"].as_array().cloned().unwrap_or_default();
        let mut raw_records = Vec::<RawRecord>::new();
        let mut malformed = false;

        for (m_idx, msg) in messages.iter().enumerate() {
            let role = match text(msg, "role") {
                "user" => "user",
                "assistant" => "assistant",
                _ => continue,
            };
            let msg_ts = parse_ts_value(msg.get("timestamp"))
                .or_else(|| parse_ts_value(msg.get("createdAt")))
                .or(fallback_ts);
            let tools = msg["tools"].as_array().cloned().unwrap_or_default();
            let parts = msg["parts"].as_array().cloned().unwrap_or_default();

            if !parts.is_empty() {
                for (p_idx, part) in parts.iter().enumerate() {
                    match text(part, "type") {
                        // Never emit internal thought / reasoning parts.
                        "thought" | "reasoning" => continue,
                        "content" | "text" => {
                            let body = first_nonempty(&[
                                text(part, "text").trim().to_owned(),
                                text(part, "content").trim().to_owned(),
                            ]);
                            if body.is_empty() {
                                continue;
                            }
                            let Some(utc_ts) = parse_ts_value(part.get("timestamp")).or(msg_ts)
                            else {
                                malformed = true;
                                continue;
                            };
                            if utc_ts.with_timezone(&timezone).date_naive() != target_day
                                || utc_ts > cutoff
                            {
                                continue;
                            }
                            raw_records.push(RawRecord {
                                order_key: format!("legacy:{m_idx:06}:{p_idx:06}:0"),
                                utc_ts,
                                kind: role.to_owned(),
                                body,
                                source: format!("{rel}:msg{}:part{}", m_idx + 1, p_idx + 1),
                                extra: None,
                            });
                        }
                        "tool-call" | "tool" => {
                            let tool_obj = part
                                .get("toolIndex")
                                .and_then(Value::as_u64)
                                .and_then(|idx| tools.get(idx as usize))
                                .unwrap_or(part);
                            emit_legacy_tool(
                                tool_obj,
                                msg_ts,
                                format!("legacy:{m_idx:06}:{p_idx:06}"),
                                format!("{rel}:msg{}:part{}", m_idx + 1, p_idx + 1),
                                target_day,
                                timezone,
                                cutoff,
                                &mut raw_records,
                            );
                        }
                        _ => {}
                    }
                }
            } else {
                let body = json_value_text(msg.get("content")).trim().to_owned();
                if !body.is_empty() {
                    if let Some(utc_ts) = msg_ts {
                        if utc_ts.with_timezone(&timezone).date_naive() == target_day
                            && utc_ts <= cutoff
                        {
                            raw_records.push(RawRecord {
                                order_key: format!("legacy:{m_idx:06}:content"),
                                utc_ts,
                                kind: role.to_owned(),
                                body,
                                source: format!("{rel}:msg{}", m_idx + 1),
                                extra: None,
                            });
                        }
                    } else {
                        malformed = true;
                    }
                }
                for (t_idx, tool_obj) in tools.iter().enumerate() {
                    emit_legacy_tool(
                        tool_obj,
                        msg_ts,
                        format!("legacy:{m_idx:06}:tool{t_idx:06}"),
                        format!("{rel}:msg{}:tool{}", m_idx + 1, t_idx + 1),
                        target_day,
                        timezone,
                        cutoff,
                        &mut raw_records,
                    );
                }
            }
        }

        if raw_records.is_empty() && !malformed {
            continue;
        }
        for rec in raw_records {
            push_record(&sid, rec, timezone, events, seen_event_ids);
        }

        let task_meta = tasks_meta.get(&raw_id);
        let project = first_nonempty(&[
            crate::common::redact(text(meta, "workspacePath").trim()),
            task_meta.map(|m| m.workspace_path.clone()).unwrap_or_default(),
        ]);
        let title = first_nonempty(&[
            crate::common::redact(text(meta, "title").trim()),
            task_meta.map(|m| m.title.clone()).unwrap_or_default(),
        ]);
        let parent_id = first_nonempty(&[
            text(meta, "forkedFromTaskId").trim().to_owned(),
            task_meta.map(|m| m.forked_from.clone()).unwrap_or_default(),
        ]);
        let mut info = json!({
            "id": sid,
            "title": title,
            "project": project,
            "warnings": [],
            "tool": TOOL,
            "toolLabel": LABEL_ZCODE,
        });
        if !parent_id.is_empty() {
            info["parentSession"] = json!(format!("{TOOL}:{parent_id}"));
            info["delegationDepth"] = json!(1);
        }
        if malformed {
            info["warnings"].as_array_mut().unwrap().push(json!(format!(
                "{rel} 含损坏或无时间戳记录，采集可能不完整"
            )));
        }
        session_info.insert(sid, info);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_legacy_tool(
    tool_obj: &Value,
    msg_ts: Option<DateTime<Utc>>,
    order_prefix: String,
    source: String,
    target_day: NaiveDate,
    timezone: Tz,
    cutoff: DateTime<Utc>,
    out: &mut Vec<RawRecord>,
) {
    let tool_name = first_nonempty(&[
        text(tool_obj, "name").trim().to_owned(),
        text(tool_obj, "toolName").trim().to_owned(),
        text(tool_obj, "title").trim().to_owned(),
    ]);
    let call_ts = parse_ts_value(tool_obj.get("startedAt")).or(msg_ts);
    let res_ts = parse_ts_value(tool_obj.get("completedAt")).or(call_ts);

    if !tool_name.is_empty() {
        if let Some(utc_ts) = call_ts {
            if utc_ts.with_timezone(&timezone).date_naive() == target_day && utc_ts <= cutoff {
                let mut body = tool_name;
                if let Some(input) = tool_obj.get("input").filter(|v| !v.is_null()) {
                    if let Ok(serialized) = serde_json::to_string(input) {
                        if !serialized.is_empty() && serialized != "{}" {
                            body.push('\n');
                            body.push_str(&serialized);
                        }
                    }
                }
                out.push(RawRecord {
                    order_key: format!("{order_prefix}:1"),
                    utc_ts,
                    kind: "tool_call".to_owned(),
                    body,
                    source: source.clone(),
                    extra: None,
                });
            }
        }
    }

    let status = text(tool_obj, "status");
    let err_msg = json_value_text(tool_obj.get("error")).trim().to_owned();
    let out_msg = json_value_text(tool_obj.get("output")).trim().to_owned();
    let is_error = status == "error" || status == "failed" || !err_msg.is_empty();
    let res_body = if is_error && !err_msg.is_empty() {
        err_msg
    } else {
        out_msg
    };
    if !res_body.is_empty() {
        if let Some(utc_ts) = res_ts {
            if utc_ts.with_timezone(&timezone).date_naive() == target_day && utc_ts <= cutoff {
                let mut extra = serde_json::Map::new();
                if is_error {
                    extra.insert("error".to_owned(), json!(true));
                }
                out.push(RawRecord {
                    order_key: format!("{order_prefix}:2"),
                    utc_ts,
                    kind: "tool_result".to_owned(),
                    body: res_body,
                    source,
                    extra: (!extra.is_empty()).then_some(extra),
                });
            }
        }
    }
}

fn push_record(
    sid: &str,
    rec: RawRecord,
    timezone: Tz,
    events: &mut Vec<Value>,
    seen_event_ids: &mut HashSet<String>,
) {
    let timestamp = rec
        .utc_ts
        .with_timezone(&timezone)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let evidence_id = format!(
        "E{}",
        &crate::common::hash(&format!("{sid}:{}:{timestamp}", rec.order_key))[..20]
    );
    if !seen_event_ids.insert(evidence_id.clone()) {
        return;
    }
    let cleaned = truncate(crate::common::redact(&rec.body));
    let mut event = json!({
        "id": evidence_id,
        "sessionId": sid,
        "timestamp": timestamp,
        "kind": rec.kind,
        "text": cleaned,
        "source": rec.source,
        "tool": TOOL,
    });
    if let Some(extra) = rec.extra {
        if let Some(obj) = event.as_object_mut() {
            for (k, v) in extra {
                obj.insert(k, v);
            }
        }
    }
    events.push(event);
}

fn first_nonempty(candidates: &[String]) -> String {
    candidates
        .iter()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_owned()
}

/// Read today's visible Z Code events and sessions under `root`.
pub(crate) fn collect(
    root: &Path,
    target_day: NaiveDate,
    timezone: Tz,
    cutoff: DateTime<Utc>,
) -> Result<super::SourceCapture> {
    let mut global_warnings = Vec::new();
    if !root.is_dir() {
        global_warnings.push(format!(
            "{LABEL_ZCODE}：记录目录不存在或不可读（{}）",
            root.display()
        ));
        return Ok(super::SourceCapture {
            sessions: Vec::new(),
            events: Vec::new(),
            global_warnings,
        });
    }

    let db_path = db_candidates(root);
    let has_v2_sessions = root.join("v2").join("sessions").is_dir();
    if db_path.is_none() && !has_v2_sessions {
        return Ok(super::SourceCapture {
            sessions: Vec::new(),
            events: Vec::new(),
            global_warnings,
        });
    }

    let work = std::env::temp_dir().join(format!("daily-report-zcode-{}", crate::common::id()));
    fs::create_dir_all(&work).context("无法创建 Z Code 临时目录")?;

    let mut ctx = CollectContext {
        target_day,
        timezone,
        cutoff,
        events: Vec::new(),
        seen_event_ids: HashSet::new(),
        session_info: HashMap::new(),
        sqlite_session_ids: HashSet::new(),
        global_warnings,
    };

    let tasks_meta = read_tasks_index(root, &work, &mut ctx.global_warnings);

    if let Some(db_path) = db_path {
        collect_sqlite_store(root, &db_path, &work, &tasks_meta, &mut ctx);
    }

    let _ = fs::remove_dir_all(&work);

    collect_legacy_snapshots(root, &tasks_meta, &mut ctx);

    let mut by_session = HashMap::<String, Vec<&Value>>::new();
    for event in &ctx.events {
        by_session
            .entry(text(event, "sessionId").to_owned())
            .or_default()
            .push(event);
    }

    let mut sessions = Vec::new();
    for (sid, mut info) in ctx.session_info {
        let mut records = by_session.get(&sid).cloned().unwrap_or_default();
        records.sort_by_key(|e| text(e, "timestamp").to_owned());
        if records.is_empty() && info["warnings"].as_array().unwrap().is_empty() {
            continue;
        }
        let existing_title = text(&info, "title").to_owned();
        let title = if !existing_title.is_empty() {
            existing_title
        } else {
            records
                .iter()
                .find(|e| text(e, "kind") == "user")
                .map(|e| {
                    text(e, "text")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        };
        info["title"] = json!(if title.is_empty() {
            format!(
                "会话 {}",
                sid.strip_prefix("zcode:")
                    .unwrap_or(&sid)
                    .chars()
                    .take(12)
                    .collect::<String>()
            )
        } else {
            title.chars().take(MAX_SESSION_TITLE).collect::<String>()
        });
        info["firstAt"] = records
            .first()
            .map(|e| e["timestamp"].clone())
            .unwrap_or(Value::Null);
        info["lastAt"] = records
            .last()
            .map(|e| e["timestamp"].clone())
            .unwrap_or(Value::Null);
        info["eventCount"] = json!(records.len());
        sessions.push(info);
    }

    Ok(super::SourceCapture {
        sessions,
        events: ctx.events,
        global_warnings: ctx.global_warnings,
    })
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
                "daily-report-zcode-{tag}-{}",
                crate::common::id()
            ));
            fs::create_dir_all(root.join("cli").join("db")).unwrap();
            fs::create_dir_all(root.join("v2").join("sessions").join("ws1")).unwrap();
            Self { root }
        }

        fn day(&self) -> String {
            crate::common::today("Asia/Shanghai").unwrap()
        }

        fn yesterday(&self) -> String {
            let today = NaiveDate::parse_from_str(&self.day(), "%Y-%m-%d").unwrap();
            (today - chrono::Duration::days(1))
                .format("%Y-%m-%d")
                .to_string()
        }

        fn db_path(&self) -> PathBuf {
            self.root.join("cli").join("db").join("db.sqlite")
        }

        fn create_sqlite(&self) -> Connection {
            let conn = Connection::open(self.db_path()).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            conn.execute_batch(
                "CREATE TABLE session (
                    id text primary key,
                    project_id text not null,
                    workspace_id text,
                    parent_id text,
                    slug text not null,
                    directory text not null,
                    path text,
                    title text not null,
                    version text not null,
                    time_created integer not null,
                    time_updated integer not null,
                    time_archived integer
                );
                CREATE TABLE message (
                    id text primary key,
                    session_id text not null,
                    time_created integer not null,
                    time_updated integer not null,
                    data text not null,
                    sequence integer
                );
                CREATE TABLE part (
                    id text primary key,
                    message_id text not null,
                    session_id text not null,
                    time_created integer not null,
                    time_updated integer not null,
                    data text not null,
                    sequence integer
                );",
            )
            .unwrap();
            conn
        }

        fn collect(&self, day: &str) -> crate::collector::SelectionCapture {
            collect_for_selection(
                &SourceRoots {
                    zcode: Some(self.root.clone()),
                    ..Default::default()
                },
                day,
                "Asia/Shanghai",
            )
            .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn ms(day: &str, hms: &str) -> i64 {
        DateTime::parse_from_rfc3339(&format!("{day}T{hms}+08:00"))
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn zcode_reads_live_wal_sqlite_parts_tools_and_excludes_reasoning() {
        let fixture = Fixture::new("wal-sqlite");
        let day = fixture.yesterday();
        let old_day = {
            let d = NaiveDate::parse_from_str(&day, "%Y-%m-%d").unwrap();
            (d - chrono::Duration::days(3))
                .format("%Y-%m-%d")
                .to_string()
        };
        let conn = fixture.create_sqlite();

        let t0 = ms(&day, "09:00:00");
        let t1 = ms(&day, "09:01:00");
        let t2 = ms(&day, "09:02:00");
        let t_old = ms(&old_day, "10:00:00");

        conn.execute(
            "INSERT INTO session (id, project_id, parent_id, slug, directory, path, title, version, time_created, time_updated)
             VALUES (?1, 'p1', '', 'slug', 'F:/WORKSPACE/demo', '', 'Z Code 构建任务', '1', ?2, ?2)",
            rusqlite::params!["sess-main", t0],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, project_id, parent_id, slug, directory, path, title, version, time_created, time_updated)
             VALUES (?1, 'p1', 'sess-main', 'slug2', 'F:/WORKSPACE/demo', '', '子任务检查', '1', ?2, ?2)",
            rusqlite::params!["sess-sub", t2],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO message VALUES ('m-old', 'sess-main', ?1, ?1, ?2, 0)",
            rusqlite::params![
                t_old,
                json!({"role": "user", "time": {"created": t_old}}).to_string()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES ('p-old', 'm-old', 'sess-main', ?1, ?1, ?2, 0)",
            rusqlite::params![
                t_old,
                json!({"type": "text", "text": "OLD_DAY_MUST_NOT_APPEAR"}).to_string()
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO message VALUES ('m-user', 'sess-main', ?1, ?1, ?2, 1)",
            rusqlite::params![
                t0,
                json!({"role": "user", "content": "FALLBACK_TEXT_FROM_DATA_CONTENT"}).to_string()
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO message VALUES ('m-asst', 'sess-main', ?1, ?1, ?2, 2)",
            rusqlite::params![
                t1,
                json!({"role": "assistant", "time": {"created": t1}}).to_string()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES ('p-think', 'm-asst', 'sess-main', ?1, ?1, ?2, 0)",
            rusqlite::params![
                t1,
                json!({"type": "reasoning", "text": "INTERNAL_REASONING_MUST_NOT_APPEAR"}).to_string()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES ('p-tool', 'm-asst', 'sess-main', ?1, ?1, ?2, 1)",
            rusqlite::params![
                t1,
                json!({
                    "type": "tool",
                    "callID": "call_1",
                    "tool": "bash",
                    "state": {
                        "status": "error",
                        "input": {"cmd": "curl -H 'Authorization: Bearer secret_token_123456'"},
                        "error": "exit status 1",
                        "time": {"start": t1, "end": t1 + 500}
                    }
                })
                .to_string()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES ('p-text', 'm-asst', 'sess-main', ?1, ?1, ?2, 2)",
            rusqlite::params![
                t1 + 1000,
                json!({"type": "text", "text": "修复命令完成", "time": {"start": t1 + 1000}}).to_string()
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO message VALUES ('m-sub', 'sess-sub', ?1, ?1, ?2, 0)",
            rusqlite::params![t2, json!({"role": "user"}).to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES ('p-sub', 'm-sub', 'sess-sub', ?1, ?1, ?2, 0)",
            rusqlite::params![
                t2,
                json!({"type": "text", "text": "执行子任务检查"}).to_string()
            ],
        )
        .unwrap();

        // Keep WAL connection open during collect to verify live WAL snapshot copying.
        let result = fixture.collect(&day);
        drop(conn);

        let dump = result.capture.to_string();
        assert!(!dump.contains("INTERNAL_REASONING_MUST_NOT_APPEAR"));
        assert!(!dump.contains("OLD_DAY_MUST_NOT_APPEAR"));
        assert!(!dump.contains("secret_token_123456"));
        assert!(dump.contains("FALLBACK_TEXT_FROM_DATA_CONTENT"));
        assert!(dump.contains("修复命令完成"));

        let events = result.capture["events"].as_array().unwrap();
        assert_eq!(events.len(), 5, "events: {events:?}");
        assert_eq!(events[0]["kind"], "user");
        assert_eq!(events[1]["kind"], "tool_call");
        assert_eq!(events[2]["kind"], "tool_result");
        assert_eq!(events[2]["error"], true);
        assert_eq!(events[3]["kind"], "assistant");
        assert_eq!(events[4]["kind"], "user");

        let sub = result
            .sessions
            .iter()
            .find(|s| s["id"] == "zcode:sess-sub")
            .unwrap();
        assert_eq!(sub["parentSession"], "zcode:sess-main");
        assert_eq!(sub["delegationDepth"], 1);
    }

    #[test]
    fn zcode_reads_legacy_snapshots_deduplicates_migrated_and_truncates() {
        let fixture = Fixture::new("legacy-dedup");
        let day = fixture.yesterday();
        let t0 = ms(&day, "14:00:00");
        let conn = fixture.create_sqlite();
        conn.execute(
            "INSERT INTO session (id, project_id, parent_id, slug, directory, path, title, version, time_created, time_updated)
             VALUES ('migrated-id', 'p1', '', 's', 'F:/repo', '', '已迁移会话', '1', ?1, ?1)",
            rusqlite::params![t0],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message VALUES ('m1', 'migrated-id', ?1, ?1, ?2, 0)",
            rusqlite::params![t0, json!({"role": "user"}).to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES ('p1', 'm1', 'migrated-id', ?1, ?1, ?2, 0)",
            rusqlite::params![
                t0,
                json!({"type": "text", "text": "FROM_SQLITE_MIGRATED"}).to_string()
            ],
        )
        .unwrap();
        drop(conn);

        // Snapshot for the SAME acpSessionId should be deduplicated and skipped.
        let dup_snap = json!({
            "meta": {
                "taskId": "legacy-dup-task",
                "acpSessionId": "migrated-id",
                "title": "旧快照",
                "workspacePath": "F:/repo",
                "updatedAt": t0
            },
            "messages": [
                {"role": "user", "content": "DUPLICATE_SNAPSHOT_MUST_NOT_APPEAR", "timestamp": t0}
            ]
        });
        fs::write(
            fixture
                .root
                .join("v2")
                .join("sessions")
                .join("ws1")
                .join("legacy-dup.json"),
            dup_snap.to_string(),
        )
        .unwrap();

        let huge = "x".repeat(super::super::MAX_EVENT_CHARS + 100);
        let legacy_only = json!({
            "meta": {
                "taskId": "legacy-only-1",
                "title": "旧版独立会话",
                "workspacePath": "F:/legacy",
                "updatedAt": t0
            },
            "messages": [
                {
                    "role": "user",
                    "content": huge,
                    "timestamp": t0
                },
                {
                    "role": "assistant",
                    "thought": "LEGACY_THOUGHT_MUST_NOT_APPEAR",
                    "timestamp": t0 + 1000,
                    "parts": [
                        {"type": "thought", "text": "LEGACY_PART_THOUGHT_SKIP"},
                        {"type": "tool-call", "toolIndex": 0},
                        {"type": "content", "text": "LEGACY_REPLY_VISIBLE"}
                    ],
                    "tools": [
                        {"name": "read_file", "status": "completed", "input": {"path": "README.md"}, "output": "ok"}
                    ]
                }
            ]
        });
        fs::write(
            fixture
                .root
                .join("v2")
                .join("sessions")
                .join("ws1")
                .join("legacy-only-1.json"),
            legacy_only.to_string(),
        )
        .unwrap();

        let result = fixture.collect(&day);
        let dump = result.capture.to_string();
        assert!(dump.contains("FROM_SQLITE_MIGRATED"));
        assert!(!dump.contains("DUPLICATE_SNAPSHOT_MUST_NOT_APPEAR"));
        assert!(!dump.contains("LEGACY_THOUGHT_MUST_NOT_APPEAR"));
        assert!(!dump.contains("LEGACY_PART_THOUGHT_SKIP"));
        assert!(dump.contains("LEGACY_REPLY_VISIBLE"));
        assert!(dump.contains("已截断"));
        assert_eq!(result.sessions.len(), 2);
    }
}
