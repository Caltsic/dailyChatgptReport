use super::{local_day_start, text, truncate, LABEL_AGY};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read},
    path::{Component, Path, PathBuf},
    time::SystemTime,
};
use walkdir::WalkDir;

const TOOL: &str = "agy";
const MAX_DB_BYTES: u64 = 200 * 1024 * 1024;
const MAX_SESSION_TITLE: usize = 100;

struct SummaryMeta {
    title: String,
    project: String,
    parent_id: String,
}

struct FullSteps {
    loaded: bool,
    by_step_index: HashMap<i64, Value>,
    by_line: HashMap<usize, Value>,
}

impl FullSteps {
    fn new() -> Self {
        Self {
            loaded: false,
            by_step_index: HashMap::new(),
            by_line: HashMap::new(),
        }
    }
}

struct RawRecord {
    line_number: usize,
    block_idx: usize,
    utc_ts: DateTime<Utc>,
    kind: String,
    body: String,
    extra: Option<serde_json::Map<String, Value>>,
}

pub(crate) fn default_root() -> PathBuf {
    std::env::var_os("AGY_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| super::home().join(".gemini").join("antigravity-cli"))
}

fn brain_root(root: &Path) -> PathBuf {
    let nested = root.join("brain");
    if nested.is_dir() {
        nested
    } else {
        root.to_owned()
    }
}

fn is_plaintext_sqlite(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut header = [0u8; 16];
    file.read_exact(&mut header).is_ok() && header == *b"SQLite format 3\0"
}

fn copy_db_snapshot(source: &Path, work: &Path) -> Result<PathBuf> {
    let size = fs::metadata(source)?.len();
    if size > MAX_DB_BYTES {
        anyhow::bail!("database too large");
    }
    let target = work.join(format!("summaries-{}.db", crate::common::id()));
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

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push(((h << 4) | l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn decode_workspace_uris(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(trimmed) else {
        return String::new();
    };
    for item in items {
        let Some(uri) = item.as_str() else {
            continue;
        };
        let uri = uri.trim();
        if uri.is_empty() {
            continue;
        }
        if let Ok(parsed) = url::Url::parse(uri) {
            if parsed.scheme() == "file" {
                if let Ok(path) = parsed.to_file_path() {
                    let s = path.to_string_lossy();
                    if !s.trim().is_empty() {
                        return crate::common::redact(&s);
                    }
                }
                let decoded = percent_decode(parsed.path());
                let stripped = if decoded.starts_with('/')
                    && decoded.as_bytes().get(2) == Some(&b':')
                {
                    &decoded[1..]
                } else {
                    decoded.as_str()
                };
                if !stripped.trim().is_empty() {
                    return crate::common::redact(stripped);
                }
            }
        }
        return crate::common::redact(uri);
    }
    String::new()
}

fn read_summaries(root: &Path, global_warnings: &mut Vec<String>) -> HashMap<String, SummaryMeta> {
    let mut map = HashMap::new();
    let db_path = root.join("conversation_summaries.db");
    if !db_path.is_file() {
        return map;
    }
    let work = std::env::temp_dir().join(format!("daily-report-agy-{}", crate::common::id()));
    if fs::create_dir_all(&work).is_err() {
        global_warnings.push(format!("{LABEL_AGY}：无法创建临时目录读取会话摘要库"));
        return map;
    }
    let res = (|| -> Result<HashMap<String, SummaryMeta>> {
        let copy = copy_db_snapshot(&db_path, &work).context("复制会话摘要库失败")?;
        if !is_plaintext_sqlite(&copy) {
            anyhow::bail!("摘要库不是有效的明文 SQLite 文件");
        }
        let conn = Connection::open_with_flags(
            &copy,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let mut out = HashMap::new();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT conversation_id, title, workspace_uris, parent_conversation_id FROM conversation_summaries",
        ) {
            let rows = stmt.query_map([], |row| {
                let id: String = row.get(0)?;
                let title: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
                let uris: String = row.get::<_, Option<String>>(2)?.unwrap_or_default();
                let parent: String = row.get::<_, Option<String>>(3)?.unwrap_or_default();
                Ok((id, title, uris, parent))
            })?;
            for row in rows.flatten() {
                let (id, title, uris, parent) = row;
                if id.trim().is_empty() {
                    continue;
                }
                out.insert(
                    id,
                    SummaryMeta {
                        title: crate::common::redact(title.trim()),
                        project: decode_workspace_uris(&uris),
                        parent_id: parent.trim().to_owned(),
                    },
                );
            }
        }
        if let Ok(mut stmt) =
            conn.prepare("SELECT conversation_id, parent_id FROM conversation_parents")
        {
            if let Ok(rows) = stmt.query_map([], |row| {
                let id: String = row.get(0)?;
                let parent: String = row.get(1)?;
                Ok((id, parent))
            }) {
                for row in rows.flatten() {
                    let (id, parent) = row;
                    if id.trim().is_empty() || parent.trim().is_empty() {
                        continue;
                    }
                    let entry = out.entry(id).or_insert_with(|| SummaryMeta {
                        title: String::new(),
                        project: String::new(),
                        parent_id: String::new(),
                    });
                    if entry.parent_id.is_empty() {
                        entry.parent_id = parent.trim().to_owned();
                    }
                }
            }
        }
        Ok(out)
    })();
    let _ = fs::remove_dir_all(&work);
    match res {
        Ok(loaded) => map = loaded,
        Err(_) => {
            global_warnings.push(format!(
                "{LABEL_AGY}：conversation_summaries.db 无法读取，已仅从对话文本恢复元数据"
            ));
        }
    }
    map
}

fn file_activity(transcript_path: &Path) -> Option<DateTime<Utc>> {
    let mut newest = None;
    let full_path = transcript_path.with_file_name("transcript_full.jsonl");
    for candidate in [transcript_path, full_path.as_path()] {
        if let Ok(modified) = fs::metadata(candidate).and_then(|meta| meta.modified()) {
            let modified: SystemTime = modified;
            let modified: DateTime<Utc> = modified.into();
            newest = Some(newest.map_or(modified, |cur: DateTime<Utc>| cur.max(modified)));
        }
    }
    newest
}

fn extract_user_prompt(raw: &str) -> String {
    if let Some(start) = raw.find("<USER_REQUEST>") {
        let after = &raw[start + "<USER_REQUEST>".len()..];
        let inner = match after.find("</USER_REQUEST>") {
            Some(end) => &after[..end],
            None => after,
        };
        return inner.trim().to_owned();
    }
    let without_meta = match raw.find("<ADDITIONAL_METADATA>") {
        Some(idx) => &raw[..idx],
        None => raw,
    };
    without_meta.trim().to_owned()
}

fn load_full_steps(transcript_path: &Path, cache: &mut FullSteps) {
    if cache.loaded {
        return;
    }
    cache.loaded = true;
    let full_path = transcript_path.with_file_name("transcript_full.jsonl");
    let Ok(file) = File::open(full_path) else {
        return;
    };
    for (idx, line) in BufReader::new(file).lines().enumerate() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line.trim_start_matches('\u{feff}')) else {
            continue;
        };
        if !row.is_object() {
            continue;
        }
        if let Some(step_idx) = row.get("step_index").and_then(Value::as_i64) {
            cache.by_step_index.insert(step_idx, row.clone());
        }
        cache.by_line.insert(idx + 1, row);
    }
}

fn hydrate_truncated(row: &mut Value, line_number: usize, cache: &mut FullSteps, transcript_path: &Path) {
    let Some(fields) = row
        .get("truncated_fields")
        .and_then(Value::as_array)
        .filter(|arr| !arr.is_empty())
        .cloned()
    else {
        return;
    };
    load_full_steps(transcript_path, cache);
    let step_idx = row.get("step_index").and_then(Value::as_i64);
    let full = step_idx
        .and_then(|idx| cache.by_step_index.get(&idx))
        .or_else(|| cache.by_line.get(&line_number));
    let Some(full_obj) = full.and_then(Value::as_object) else {
        return;
    };
    let Some(row_obj) = row.as_object_mut() else {
        return;
    };
    for field in fields {
        let Some(key) = field.as_str() else {
            continue;
        };
        // Never hydrate or surface internal reasoning text.
        if key == "thinking" {
            continue;
        }
        if let Some(val) = full_obj.get(key) {
            row_obj.insert(key.to_owned(), val.clone());
        }
    }
}

fn conversation_id_from_path(scan_root: &Path, path: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(scan_root) {
        if let Some(Component::Normal(first)) = rel.components().next() {
            let s = first.to_string_lossy();
            if s != "transcript.jsonl" && s != ".system_generated" {
                return s.into_owned();
            }
        }
    }
    let fallback_rel = path
        .strip_prefix(scan_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    crate::common::hash(&fallback_rel)[..16].to_owned()
}

/// Read today's visible AGY CLI events and sessions under `root`.
pub(crate) fn collect(
    root: &Path,
    target_day: NaiveDate,
    timezone: Tz,
    cutoff: DateTime<Utc>,
) -> Result<super::SourceCapture> {
    let mut global_warnings = Vec::new();
    let scan_root = brain_root(root);
    if !scan_root.is_dir() {
        global_warnings.push(format!(
            "{LABEL_AGY}：记录目录不存在或不可读（{}）",
            root.display()
        ));
        return Ok(super::SourceCapture {
            sessions: Vec::new(),
            events: Vec::new(),
            global_warnings,
        });
    }

    let summaries = read_summaries(root, &mut global_warnings);
    let day_start = local_day_start(target_day, timezone);

    let mut files = Vec::new();
    for entry in WalkDir::new(&scan_root).max_depth(5).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                global_warnings.push(format!(
                    "{LABEL_AGY}：无法扫描部分对话目录，采集不完整"
                ));
                continue;
            }
        };
        if !entry.file_type().is_file()
            || entry.file_name().to_string_lossy() != "transcript.jsonl"
        {
            continue;
        }
        let path = entry.path().to_owned();
        if let Some(start) = day_start {
            if let Some(modified) = file_activity(&path) {
                if modified + chrono::Duration::hours(1) < start {
                    continue;
                }
            }
        }
        files.push(path);
    }
    files.sort();

    let mut events = Vec::new();
    let mut seen_event_ids = HashSet::new();
    let mut session_info = HashMap::<String, Value>::new();

    for path in files {
        let rel = crate::common::redact(
            &path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy(),
        );
        let conv_id = conversation_id_from_path(&scan_root, &path);
        let session_id = format!("{TOOL}:{conv_id}");

        let stream = match File::open(&path) {
            Ok(stream) => stream,
            Err(_) => {
                session_info.insert(
                    session_id.clone(),
                    json!({
                        "id": session_id,
                        "title": "",
                        "project": "",
                        "warnings": [format!("{LABEL_AGY}：无法读取 {rel}，采集不完整")],
                        "tool": TOOL,
                        "toolLabel": LABEL_AGY,
                    }),
                );
                continue;
            }
        };

        let mut malformed = false;
        let mut raw_records = Vec::<RawRecord>::new();
        let mut full_cache = FullSteps::new();

        for (line_index, line) in BufReader::new(stream).lines().enumerate() {
            let line_number = line_index + 1;
            let line = match line {
                Ok(line) => line,
                Err(_) => {
                    malformed = true;
                    break;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let mut row: Value = match serde_json::from_str(line.trim_start_matches('\u{feff}')) {
                Ok(Value::Object(map)) => Value::Object(map),
                _ => {
                    malformed = true;
                    continue;
                }
            };

            let source = text(&row, "source").to_owned();
            let step_type = text(&row, "type").to_owned();
            if source == "SYSTEM"
                || !matches!(
                    (source.as_str(), step_type.as_str()),
                    ("USER_EXPLICIT", "USER_INPUT")
                        | ("MODEL", "PLANNER_RESPONSE")
                        | ("MODEL", "GENERIC")
                )
            {
                continue;
            }

            let Ok(parsed_ts) = DateTime::parse_from_rfc3339(text(&row, "created_at")) else {
                malformed = true;
                continue;
            };
            let utc_ts = parsed_ts.with_timezone(&Utc);
            if parsed_ts.with_timezone(&timezone).date_naive() != target_day || utc_ts > cutoff {
                continue;
            }

            hydrate_truncated(&mut row, line_number, &mut full_cache, &path);

            match (source.as_str(), step_type.as_str()) {
                ("USER_EXPLICIT", "USER_INPUT") => {
                    let body = extract_user_prompt(text(&row, "content"));
                    if !body.is_empty() {
                        raw_records.push(RawRecord {
                            line_number,
                            block_idx: 0,
                            utc_ts,
                            kind: "user".to_owned(),
                            body,
                            extra: None,
                        });
                    }
                }
                ("MODEL", "PLANNER_RESPONSE") => {
                    // Explicitly skip `thinking`; only collect tool calls and assistant text.
                    let mut next_block = 0usize;
                    if let Some(calls) = row.get("tool_calls").and_then(Value::as_array) {
                        for (idx, call) in calls.iter().enumerate() {
                            let name = text(call, "name").trim();
                            if name.is_empty() {
                                continue;
                            }
                            let mut body = name.to_owned();
                            if let Some(args) = call.get("args").filter(|v| !v.is_null()) {
                                if let Ok(serialized) = serde_json::to_string(args) {
                                    if !serialized.is_empty() && serialized != "{}" {
                                        body.push('\n');
                                        body.push_str(&serialized);
                                    }
                                }
                            }
                            raw_records.push(RawRecord {
                                line_number,
                                block_idx: idx,
                                utc_ts,
                                kind: "tool_call".to_owned(),
                                body,
                                extra: None,
                            });
                            next_block = idx + 1;
                        }
                    }
                    let reply = text(&row, "content").trim();
                    if !reply.is_empty() {
                        raw_records.push(RawRecord {
                            line_number,
                            block_idx: next_block,
                            utc_ts,
                            kind: "assistant".to_owned(),
                            body: reply.to_owned(),
                            extra: None,
                        });
                    }
                }
                ("MODEL", "GENERIC") => {
                    let content = text(&row, "content").trim();
                    let err_text = text(&row, "error").trim();
                    let body = if !content.is_empty() {
                        content
                    } else {
                        err_text
                    };
                    if !body.is_empty() {
                        let mut extra = serde_json::Map::new();
                        if text(&row, "status") == "ERROR" {
                            extra.insert("error".to_owned(), json!(true));
                        }
                        raw_records.push(RawRecord {
                            line_number,
                            block_idx: 0,
                            utc_ts,
                            kind: "tool_result".to_owned(),
                            body: body.to_owned(),
                            extra: (!extra.is_empty()).then_some(extra),
                        });
                    }
                }
                _ => {}
            }
        }

        for RawRecord {
            line_number,
            block_idx,
            utc_ts,
            kind,
            body,
            extra,
        } in raw_records
        {
            let timestamp = utc_ts
                .with_timezone(&timezone)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let evidence_id = format!(
                "E{}",
                &crate::common::hash(&format!(
                    "{session_id}:{line_number}:{block_idx}:{timestamp}"
                ))[..20]
            );
            if !seen_event_ids.insert(evidence_id.clone()) {
                continue;
            }
            let cleaned = truncate(crate::common::redact(&body));
            let mut event = json!({
                "id": evidence_id,
                "sessionId": session_id,
                "timestamp": timestamp,
                "kind": kind,
                "text": cleaned,
                "source": format!("{rel}:{line_number}"),
                "tool": TOOL,
            });
            if let Some(extra) = extra {
                if let Some(obj) = event.as_object_mut() {
                    for (k, v) in extra {
                        obj.insert(k, v);
                    }
                }
            }
            events.push(event);
        }

        let mut local_warnings = Vec::new();
        if malformed {
            local_warnings.push(format!(
                "{rel} 含损坏、未写完或无时间戳的记录；无法保证完整，稍后可重交"
            ));
        }

        let meta = summaries.get(&conv_id);
        let mut info = session_info
            .remove(&session_id)
            .unwrap_or_else(|| {
                json!({
                    "id": session_id,
                    "title": "",
                    "project": "",
                    "warnings": [],
                    "tool": TOOL,
                    "toolLabel": LABEL_AGY,
                })
            });
        if let Some(meta) = meta {
            if !meta.title.is_empty() {
                info["title"] = json!(meta.title);
            }
            if !meta.project.is_empty() {
                info["project"] = json!(meta.project);
            }
            if !meta.parent_id.is_empty() {
                info["parentSession"] = json!(format!("{TOOL}:{}", meta.parent_id));
                info["delegationDepth"] = json!(1);
            }
        }
        let attached = info["warnings"].as_array_mut().unwrap();
        for warning in local_warnings {
            let value = json!(crate::common::redact(&warning));
            if !attached.contains(&value) {
                attached.push(value);
            }
        }
        session_info.insert(session_id, info);
    }

    let mut by_session = HashMap::<String, Vec<&Value>>::new();
    for event in &events {
        by_session
            .entry(text(event, "sessionId").to_owned())
            .or_default()
            .push(event);
    }

    let mut sessions = Vec::new();
    for (sid, mut info) in session_info {
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
                sid.strip_prefix("agy:")
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
        events,
        global_warnings,
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
                "daily-report-agy-{tag}-{}",
                crate::common::id()
            ));
            fs::create_dir_all(root.join("brain")).unwrap();
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

        fn transcript_dir(&self, conv_id: &str) -> PathBuf {
            let dir = self
                .root
                .join("brain")
                .join(conv_id)
                .join(".system_generated")
                .join("logs");
            fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn write_transcript(&self, conv_id: &str, name: &str, lines: &[Value]) {
            let dir = self.transcript_dir(conv_id);
            let mut content = String::new();
            for line in lines {
                content.push_str(&line.to_string());
                content.push('\n');
            }
            fs::write(dir.join(name), content).unwrap();
        }

        fn collect(&self, day: &str) -> crate::collector::SelectionCapture {
            collect_for_selection(
                &SourceRoots {
                    agy: Some(self.root.clone()),
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

    fn iso(day: &str, hms: &str) -> String {
        format!("{day}T{hms}+08:00")
    }

    #[test]
    fn agy_reads_visible_records_hydrates_truncated_and_skips_thinking_and_system() {
        let fixture = Fixture::new("core");
        let day = fixture.yesterday();
        let conv = "conv-1111";

        let compact = vec![
            json!({
                "step_index": 0,
                "source": "SYSTEM",
                "type": "SYSTEM_MESSAGE",
                "status": "DONE",
                "created_at": iso(&day, "09:00:00"),
                "content": "INTERNAL_SYSTEM_MUST_NOT_APPEAR"
            }),
            json!({
                "step_index": 1,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": iso(&day, "09:01:00"),
                "content": "<USER_REQUEST>\nUSER_VISIBLE_REQUEST 构建发布包\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nLOCAL_META_SKIP\n</ADDITIONAL_METADATA>"
            }),
            json!({
                "step_index": 2,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": iso(&day, "09:02:00"),
                "thinking": "PRIVATE_THINKING_MUST_NOT_APPEAR",
                "tool_calls": [
                    {"name": "run_command", "args": {"CommandLine": "cargo test --token sk-abcdefghijklmnopqrst"}}
                ],
                "content": "ASSISTANT_PLAN_VISIBLE"
            }),
            json!({
                "step_index": 3,
                "source": "MODEL",
                "type": "GENERIC",
                "status": "ERROR",
                "created_at": iso(&day, "09:03:00"),
                "content": "TRUNCATED...",
                "truncated_fields": ["content"]
            }),
            json!({
                "step_index": 4,
                "source": "SYSTEM",
                "type": "CHECKPOINT",
                "status": "DONE",
                "created_at": iso(&day, "09:04:00"),
                "content": "CHECKPOINT_MUST_NOT_APPEAR"
            }),
        ];
        let full = vec![
            compact[0].clone(),
            compact[1].clone(),
            compact[2].clone(),
            json!({
                "step_index": 3,
                "source": "MODEL",
                "type": "GENERIC",
                "status": "ERROR",
                "created_at": iso(&day, "09:03:00"),
                "content": "FULL_TOOL_RESULT_HYDRATED 编译失败详情"
            }),
            compact[4].clone(),
        ];

        fixture.write_transcript(conv, "transcript.jsonl", &compact);
        fixture.write_transcript(conv, "transcript_full.jsonl", &full);

        let result = fixture.collect(&day);
        let text_dump = result.capture.to_string();
        assert!(!text_dump.contains("INTERNAL_SYSTEM_MUST_NOT_APPEAR"));
        assert!(!text_dump.contains("LOCAL_META_SKIP"));
        assert!(!text_dump.contains("PRIVATE_THINKING_MUST_NOT_APPEAR"));
        assert!(!text_dump.contains("CHECKPOINT_MUST_NOT_APPEAR"));
        assert!(!text_dump.contains("sk-abcdefghijklmnopqrst"));
        assert!(text_dump.contains("USER_VISIBLE_REQUEST 构建发布包"));
        assert!(text_dump.contains("FULL_TOOL_RESULT_HYDRATED 编译失败详情"));
        assert!(!text_dump.contains("TRUNCATED..."));

        let events = result.capture["events"].as_array().unwrap();
        assert_eq!(events.len(), 4, "events: {events:?}");
        assert_eq!(events[0]["kind"], "user");
        assert_eq!(events[0]["text"], "USER_VISIBLE_REQUEST 构建发布包");
        assert_eq!(events[1]["kind"], "tool_call");
        assert_eq!(events[2]["kind"], "assistant");
        assert_eq!(events[3]["kind"], "tool_result");
        assert_eq!(events[3]["error"], true);
        assert_eq!(result.sessions.len(), 1);
        assert_eq!(result.sessions[0]["tool"], "agy");
        assert_eq!(result.sessions[0]["toolLabel"], LABEL_AGY);
    }

    #[test]
    fn agy_reads_live_wal_summaries_db_project_title_and_parent_link() {
        let fixture = Fixture::new("wal-db");
        let day = fixture.yesterday();
        let parent_id = "conv-parent";
        let child_id = "conv-child";

        let db_path = fixture.root.join("conversation_summaries.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute(
            "CREATE TABLE conversation_summaries (
                conversation_id TEXT PRIMARY KEY,
                title TEXT,
                workspace_uris TEXT,
                parent_conversation_id TEXT
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_summaries VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                parent_id,
                "整理项目归档",
                r#"["file:///F:/WORKSPACE/codex%E5%B7%A5%E4%BD%9C%E6%B1%87%E6%80%BB"]"#,
                ""
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_summaries VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                child_id,
                "子代理调研",
                r#"["file:///F:/WORKSPACE/codex%E5%B7%A5%E4%BD%9C%E6%B1%87%E6%80%BB"]"#,
                parent_id
            ],
        )
        .unwrap();

        // Keep `conn` open in WAL mode while collecting to verify WAL snapshot safety.
        fixture.write_transcript(
            parent_id,
            "transcript.jsonl",
            &[json!({
                "step_index": 0,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "created_at": iso(&day, "10:00:00"),
                "content": "开始整理"
            })],
        );
        fixture.write_transcript(
            child_id,
            "transcript.jsonl",
            &[json!({
                "step_index": 0,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "created_at": iso(&day, "10:05:00"),
                "content": "子代理检查细节"
            })],
        );

        let result = fixture.collect(&day);
        drop(conn);

        assert_eq!(result.sessions.len(), 2);
        let child = result
            .sessions
            .iter()
            .find(|s| s["id"] == format!("agy:{child_id}"))
            .unwrap();
        assert_eq!(child["title"], "子代理调研");
        assert_eq!(child["parentSession"], format!("agy:{parent_id}"));
        assert_eq!(child["delegationDepth"], 1);
        assert!(child["project"]
            .as_str()
            .unwrap()
            .contains("F:/WORKSPACE/codex工作汇总")
            || child["project"]
                .as_str()
                .unwrap()
                .contains("F:\\WORKSPACE\\codex工作汇总"));
    }

    #[test]
    fn agy_filters_target_day_and_truncates_oversized_text() {
        let fixture = Fixture::new("day-trunc");
        let day = fixture.yesterday();
        let old_day = {
            let d = NaiveDate::parse_from_str(&day, "%Y-%m-%d").unwrap();
            (d - chrono::Duration::days(5))
                .format("%Y-%m-%d")
                .to_string()
        };
        let huge = "a".repeat(super::super::MAX_EVENT_CHARS + 250);

        fixture.write_transcript(
            "conv-mixed",
            "transcript.jsonl",
            &[
                json!({
                    "step_index": 0,
                    "source": "USER_EXPLICIT",
                    "type": "USER_INPUT",
                    "created_at": iso(&old_day, "10:00:00"),
                    "content": "OLD_DAY_SKIP"
                }),
                json!({
                    "step_index": 1,
                    "source": "USER_EXPLICIT",
                    "type": "USER_INPUT",
                    "created_at": iso(&day, "11:00:00"),
                    "content": huge
                }),
            ],
        );

        let result = fixture.collect(&day);
        let events = result.capture["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        let body = events[0]["text"].as_str().unwrap();
        assert!(body.contains("已截断"));
        assert!(!result.capture.to_string().contains("OLD_DAY_SKIP"));
    }
}
