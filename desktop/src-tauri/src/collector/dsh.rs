use super::{block_text, local_day_start, text, truncate, LABEL_DSH};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

const TOOL: &str = "dsh";
/// Session format this collector understands. DSH upgrades its persisted format and
/// migrates older generations internally, so unknown versions are skipped with a note.
const FORMAT_VERSION: u64 = 4;

/// DSH internal record types that never describe visible work.
const IGNORED_TYPES: &[&str] = &[
    "request/header",
    "request/context",
    "system/message",
    "developer/message",
    "assistant/attempt",
    "agent/inbox/spliced",
    "subagent/catalog",
    "session/end-seed",
    "session/title-llm-request",
    "session-log-deepseek/delivery-accepted",
    "team/message/queued",
    "deliverables/presented",
    "workspace/changes",
    "tool/ptc-dispatch",
    "tool/ptc-dispatch-start",
    "web/deepseek-search-llm-request",
];

/// Any other first-party namespace is internal bookkeeping rather than work content.
const IGNORED_PREFIXES: &[&str] = &[
    "permission/",
    "sandbox/",
    "approval/",
    "command/",
    "turn/",
    "step/",
    "llm/",
    "compaction/",
    "goal/",
    "plan/",
    "todo/",
    "web/",
    "session-log-",
    "plugin/",
];

type Mapped = (String, String, Option<serde_json::Map<String, Value>>);

struct SessionFile {
    path: PathBuf,
    version: u64,
    compressed: bool,
}

/// A configured DSH path may be the DSH home (containing `sessions/`) or the
/// persistence root itself when the user moved it.
fn sessions_root(root: &Path) -> PathBuf {
    let nested = root.join("sessions");
    if nested.is_dir() {
        return nested;
    }
    if root.is_dir() {
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("--") || name == "_no-cwd" {
                    return root.to_owned();
                }
            }
        }
    }
    nested
}

fn generation(name: &str) -> Option<(u64, bool)> {
    let (stem, compressed) = match name.strip_suffix(".zstd") {
        Some(stem) => (stem, true),
        None => (name, false),
    };
    if let Some(rest) = stem.strip_prefix("session.v") {
        let version = rest.strip_suffix(".jsonl")?;
        return version.parse().ok().map(|version| (version, compressed));
    }
    (stem == "session.jsonl").then_some((0, compressed))
}

fn discover(root: &Path, warnings: &mut Vec<String>) -> Vec<SessionFile> {
    let mut grouped: HashMap<PathBuf, Vec<(u64, bool, PathBuf)>> = HashMap::new();
    for entry in WalkDir::new(root).min_depth(2).max_depth(3).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                warnings.push(format!("{LABEL_DSH}：无法扫描部分记录目录，采集不完整"));
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some((version, compressed)) = generation(name) else {
            continue;
        };
        let Some(parent) = path.parent() else {
            continue;
        };
        grouped
            .entry(parent.to_owned())
            .or_default()
            .push((version, compressed, path.to_owned()));
    }
    let mut files = Vec::new();
    for candidates in grouped.into_values() {
        // DSH reads the highest persisted generation; one encoding is used per root.
        let mut best: Option<(u64, bool, PathBuf)> = None;
        for candidate in candidates {
            let replace = match &best {
                None => true,
                Some(current) => {
                    candidate.0 > current.0 || (candidate.0 == current.0 && candidate.1 && !current.1)
                }
            };
            if replace {
                best = Some(candidate);
            }
        }
        if let Some((version, compressed, path)) = best {
            files.push(SessionFile {
                path,
                version,
                compressed,
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

fn reader(file: &SessionFile) -> Result<Box<dyn BufRead>> {
    let stream = File::open(&file.path)
        .with_context(|| format!("无法读取 {}", file.path.display()))?;
    if file.compressed {
        // Decoder reads the concatenated Zstandard frames DSH appends per batch.
        Ok(Box::new(BufReader::new(zstd::stream::read::Decoder::new(
            stream,
        )?)))
    } else {
        Ok(Box::new(BufReader::new(stream)))
    }
}

fn read_header(file: &SessionFile) -> Result<Value> {
    for line in reader(file)?.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let row: Value = serde_json::from_str(line.trim_start_matches('\u{feff}'))?;
        if text(&row, "type") == "session" {
            return Ok(row);
        }
        anyhow::bail!("会话文件缺少 header");
    }
    anyhow::bail!("会话文件为空")
}

fn timestamp_of(row: &Value) -> Option<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(row.get("time")?.as_i64()?)
}

fn ignored(row_type: &str) -> bool {
    IGNORED_TYPES.contains(&row_type)
        || IGNORED_PREFIXES
            .iter()
            .any(|prefix| row_type.starts_with(prefix))
}

fn map_user(data: &Value) -> Option<Mapped> {
    let kind = data
        .get("source")
        .and_then(|source| source.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let body = block_text(data.get("content"));
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    match kind {
        // Human input only; runtime context, skill catalogs, approvals and teammate
        // notifications are injected as user messages but are not work records.
        "user" => Some(("user".to_owned(), body.to_owned(), None)),
        "agent-message" => Some((
            "user".to_owned(),
            format!("协作消息 · 来源：其他代理\n{body}"),
            None,
        )),
        _ => None,
    }
}

fn map_assistant(data: &Value) -> Option<Mapped> {
    let message = data.get("message")?;
    let blocks = message.get("content")?.as_array()?;
    let mut texts = Vec::new();
    for block in blocks {
        if text(block, "type") == "text" {
            let body = text(block, "text");
            if !body.trim().is_empty() {
                texts.push(body.to_owned());
            }
        }
    }
    if texts.is_empty() {
        return None;
    }
    let mut extra = serde_json::Map::new();
    if data.get("interrupted").and_then(Value::as_bool) == Some(true) {
        extra.insert("interrupted".to_owned(), json!(true));
    }
    Some((
        "assistant".to_owned(),
        texts.join("\n"),
        (!extra.is_empty()).then_some(extra),
    ))
}

fn map_tool_call(data: &Value) -> Option<Mapped> {
    let name = text(data, "name").trim();
    if name.is_empty() {
        return None;
    }
    let mut body = name.to_owned();
    let arguments = text(data, "arguments");
    if !arguments.trim().is_empty() {
        body.push('\n');
        body.push_str(arguments);
    }
    Some(("tool_call".to_owned(), body, None))
}

fn map_tool_result(data: &Value) -> Option<Mapped> {
    let message = data.get("message")?;
    let mut texts = Vec::new();
    let mut images = 0usize;
    for block in message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "text" => {
                let body = text(block, "text");
                if !body.trim().is_empty() {
                    texts.push(body.to_owned());
                }
            }
            "image" => images += 1,
            _ => {}
        }
    }
    if images > 0 {
        texts.push(format!("[非文本工具结果未采集：{images} 项]"));
    }
    let body = texts.join("\n");
    if body.trim().is_empty() {
        return None;
    }
    let mut extra = serde_json::Map::new();
    if message.get("isError").and_then(Value::as_bool) == Some(true) {
        extra.insert("error".to_owned(), json!(true));
    }
    Some((
        "tool_result".to_owned(),
        body,
        (!extra.is_empty()).then_some(extra),
    ))
}

/// Read the visible work records of one DSH home for a single department day.
pub(crate) fn collect(
    root: &Path,
    target_day: NaiveDate,
    timezone: Tz,
    cutoff: DateTime<Utc>,
) -> Result<super::SourceCapture> {
    let sessions_root = sessions_root(root);
    let mut global_warnings = Vec::new();
    if !sessions_root.is_dir() {
        global_warnings.push(format!(
            "{LABEL_DSH}：记录目录不存在或不可读（{}）",
            root.display()
        ));
        return Ok(super::SourceCapture {
            sessions: Vec::new(),
            events: Vec::new(),
            global_warnings,
        });
    }
    let files = discover(&sessions_root, &mut global_warnings);
    let day_start = local_day_start(target_day, timezone);
    let created_cutoff = target_day
        .succ_opt()
        .and_then(|next| local_day_start(next, timezone))
        .map(|end| end.timestamp_millis());
    let mut sessions = Vec::new();
    let mut events = Vec::new();

    for file in files {
        // Appends never predate the file modification time, so an untouched log cannot
        // contain target-day events. Keep one hour of slack for clock changes.
        if let Some(start) = day_start {
            if let Ok(modified) = std::fs::metadata(&file.path).and_then(|metadata| metadata.modified()) {
                let modified: DateTime<Utc> = modified.into();
                if modified + chrono::Duration::hours(1) < start {
                    continue;
                }
            }
        }
        let rel = crate::common::redact(
            &file
                .path
                .strip_prefix(root)
                .unwrap_or(&file.path)
                .to_string_lossy(),
        );
        if file.version != FORMAT_VERSION {
            let label = if file.version < FORMAT_VERSION {
                "旧版"
            } else {
                "较新"
            };
            global_warnings.push(format!(
                "{LABEL_DSH}：跳过{label}会话格式 v{}（{rel}），仅采集 v{FORMAT_VERSION}",
                file.version
            ));
            continue;
        }
        let header = match read_header(&file) {
            Ok(header) => header,
            Err(_) => {
                global_warnings.push(format!(
                    "{LABEL_DSH}：无法读取会话文件 {rel}，采集不完整"
                ));
                continue;
            }
        };
        let raw_id = text(&header, "id").to_owned();
        if raw_id.is_empty() {
            global_warnings.push(format!(
                "{LABEL_DSH}：会话文件 {rel} 缺少会话编号，未采集"
            ));
            continue;
        }
        if let Some(limit) = created_cutoff {
            if header
                .get("createdAt")
                .and_then(Value::as_i64)
                .is_some_and(|created| created >= limit)
            {
                // Created after the target day; it cannot contain that day's events.
                continue;
            }
        }
        let session_id = format!("{TOOL}:{raw_id}");
        let is_seeded = header
            .get("isSeeded")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut session = json!({"id":session_id,"title":"",
            "project":crate::common::redact(text(&header, "cwd")),"warnings":[],
            "tool":TOOL,"toolLabel":LABEL_DSH});
        if let Some(parent) = header
            .get("parentSession")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            session["parentSession"] = json!(format!("{TOOL}:{parent}"));
        }
        if let Some(origin) = header
            .get("origin")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            session["origin"] = json!(origin);
        }
        if let Some(depth) = header.get("delegationDepth").and_then(Value::as_u64) {
            session["delegationDepth"] = json!(depth);
        }

        let mut session_warnings = Vec::<String>::new();
        let mut session_events = Vec::<Value>::new();
        let mut pending_events = Vec::<Value>::new();
        // Seeded sessions inherit history from a parent. Events before the last tagged
        // end-seed marker are inherited and must never be counted as this day's work.
        let mut inherited_marker = !is_seeded;
        let mut malformed = 0usize;
        let mut torn = false;
        let mut unsupported = HashSet::<String>::new();
        let mut title: Option<String> = None;
        let mut descriptor_label: Option<String> = None;

        let mut lines = reader(&file)?.lines();
        let mut line_index = 0usize;
        loop {
            let line = match lines.next() {
                None => break,
                Some(Ok(line)) => line,
                Some(Err(_)) => {
                    torn = true;
                    break;
                }
            };
            line_index += 1;
            let line_number = line_index;
            if line.trim().is_empty() {
                continue;
            }
            let row: Value = match serde_json::from_str(line.trim_start_matches('\u{feff}')) {
                Ok(row) => row,
                Err(_) => {
                    malformed += 1;
                    continue;
                }
            };
            let row_type = text(&row, "type");
            if row_type == "session" {
                continue;
            }
            let data = row.get("data").cloned().unwrap_or(Value::Null);
            if row_type == "session/end-seed" {
                if data.get("inherited").and_then(Value::as_bool) == Some(true) {
                    inherited_marker = true;
                    pending_events.clear();
                }
                continue;
            }
            // Titles and subagent labels live on their own records and are session
            // metadata; capture them even when the record predates the target day.
            if row_type == "session/title" {
                if let Some(value) = data
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                {
                    title = Some(crate::common::redact(value));
                }
                continue;
            }
            if row_type == "subagent/descriptor" {
                if let Some(value) = data
                    .get("label")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                {
                    descriptor_label = Some(crate::common::redact(value));
                }
                continue;
            }
            let Some(at) = timestamp_of(&row) else {
                malformed += 1;
                continue;
            };
            if !inherited_marker || at > cutoff {
                continue;
            }
            if at.with_timezone(&timezone).date_naive() != target_day {
                continue;
            }
            let mapped = match row_type {
                "user/message" => map_user(&data),
                "assistant/message" => map_assistant(&data),
                "tool/call" => map_tool_call(&data),
                "tool/result" => map_tool_result(&data),
                other => {
                    if !ignored(other) {
                        unsupported.insert(other.to_owned());
                    }
                    None
                }
            };
            let Some((kind, body, extra)) = mapped else {
                continue;
            };
            let stamp = at
                .with_timezone(&timezone)
                .to_rfc3339_opts(SecondsFormat::Millis, false);
            let evidence_id = format!(
                "E{}",
                &crate::common::hash(&format!("{session_id}:{line_number}:{stamp}"))[..20]
            );
            let mut evidence = json!({"id":evidence_id,"sessionId":session_id,"timestamp":stamp,
                "kind":kind,"text":truncate(crate::common::redact(&body)),
                "source":format!("{rel}:{line_number}"),"tool":TOOL});
            if let Some(extra) = extra {
                if let Some(object) = evidence.as_object_mut() {
                    object.extend(extra);
                }
            }
            if is_seeded {
                pending_events.push(evidence);
            } else {
                session_events.push(evidence);
            }
        }
        if torn {
            session_warnings.push(format!(
                "{rel} 尾部记录仍在写入或已损坏，仅采集完整部分"
            ));
        }
        if malformed > 0 {
            session_warnings.push(format!(
                "{rel} 含 {malformed} 行无法解析的记录，采集可能不完整"
            ));
        }
        if !unsupported.is_empty() {
            let mut list: Vec<String> = unsupported.into_iter().collect();
            list.sort();
            let shown = list.iter().take(5).cloned().collect::<Vec<_>>().join("、");
            session_warnings.push(format!(
                "{rel} 存在未采集的事件类型 {shown}{}，相关动作未计入",
                if list.len() > 5 { " 等" } else { "" }
            ));
        }
        if is_seeded {
            if inherited_marker {
                session_events = pending_events;
            } else {
                // Without the tagged marker the inherited prefix cannot be separated
                // from this day's work, so the session is not reported at all.
                global_warnings.push(format!(
                    "{LABEL_DSH}：继承会话 {rel} 找不到继承截点，未采入当日报告"
                ));
                continue;
            }
        }
        session_events.sort_by_cached_key(|event| {
            DateTime::parse_from_rfc3339(text(event, "timestamp"))
                .map(|value| value.timestamp_micros())
                .unwrap_or(i64::MAX)
        });
        if session_events.is_empty() && session_warnings.is_empty() {
            continue;
        }
        let title = title.or(descriptor_label).unwrap_or_default();
        let title = if title.is_empty() {
            session_events
                .iter()
                .find(|event| text(event, "kind") == "user")
                .map(|event| {
                    text(event, "text")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        } else {
            title
        };
        let title = if title.is_empty() {
            format!("会话 {}", raw_id.chars().take(12).collect::<String>())
        } else {
            title.chars().take(100).collect::<String>()
        };
        session["title"] = json!(title);
        session["warnings"] = json!(session_warnings);
        session["firstAt"] = session_events
            .first()
            .map(|event| event["timestamp"].clone())
            .unwrap_or(Value::Null);
        session["lastAt"] = session_events
            .last()
            .map(|event| event["timestamp"].clone())
            .unwrap_or(Value::Null);
        session["eventCount"] = json!(session_events.len());
        events.extend(session_events);
        sessions.push(session);
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
    use crate::collector::{collect_for_selection, MAX_EVENT_CHARS, SourceRoots};

    fn now_ms() -> i64 {
        Utc::now().timestamp_millis()
    }

    fn header(id: &str, created_at: i64, seeded: bool) -> Value {
        json!({"type":"session","version":4,"id":id,"createdAt":created_at,
            "cwd":"C:\\Workspace\\Demo","isSeeded":seeded,"delegationDepth":0,"agentPreset":"standard"})
    }

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "daily-report-dsh-{tag}-{}",
                crate::common::id()
            ));
            std::fs::create_dir_all(root.join("sessions/project")).unwrap();
            Self { root }
        }
        fn write(&self, id: &str, header: Value, rows: &[Value], compressed: bool) -> PathBuf {
            let dir = self.root.join("sessions/project").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(if compressed {
                "session.v4.jsonl.zstd"
            } else {
                "session.v4.jsonl"
            });
            let mut lines = vec![serde_json::to_string(&header).unwrap()];
            lines.extend(rows.iter().map(|row| serde_json::to_string(row).unwrap()));
            if compressed {
                let mut bytes = Vec::new();
                for chunk in [&lines[..1], &lines[1..]] {
                    if chunk.is_empty() {
                        continue;
                    }
                    let payload = format!("{}\n", chunk.join("\n"));
                    bytes.extend(zstd::encode_all(payload.as_bytes(), 3).unwrap());
                }
                std::fs::write(&path, bytes).unwrap();
            } else {
                std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
            }
            path
        }
        fn day(&self) -> String {
            crate::common::today("Asia/Shanghai").unwrap()
        }
        fn collect(&self) -> crate::collector::SelectionCapture {
            collect_for_selection(
                &SourceRoots {
                    codex: None,
                    dsh: Some(self.root.clone()),
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

    fn message(seq: u64, time: i64, text: &str, source: &str) -> Value {
        json!({"type":"user/message","seq":seq,"time":time,
            "data":{"content":[{"type":"text","text":text}],"source":{"kind":source},
                "role":"user","id":format!("m{seq}")}})
    }

    fn assistant(seq: u64, time: i64, reasoning: &str, text: &str, tool_calls: usize) -> Value {
        let mut content = vec![json!({"type":"reasoning","text":reasoning})];
        for call in 0..tool_calls {
            content.push(json!({"type":"tool-call","callId":format!("c{call}"),"name":"pwsh","arguments":"{}"}));
        }
        if !text.is_empty() {
            content.push(json!({"type":"text","text":text}));
        }
        json!({"type":"assistant/message","seq":seq,"time":time,
            "data":{"turn":1,"step":1,"message":{"role":"assistant","content":content,
                "source":{"kind":"model","provider":"test","model":"test"},"id":format!("a{seq}")}}})
    }

    fn tool_call(seq: u64, time: i64, call: &str, name: &str, arguments: &str) -> Value {
        json!({"type":"tool/call","seq":seq,"time":time,
            "data":{"turn":1,"step":1,"callId":call,"name":name,"arguments":arguments}})
    }

    fn tool_result(seq: u64, time: i64, call: &str, text: &str, error: bool) -> Value {
        json!({"type":"tool/result","seq":seq,"time":time,
            "data":{"turn":1,"step":1,"message":{"role":"tool","source":{"kind":"tool","callId":call},
                "toolCallId":call,"content":[{"type":"text","text":text}],"isError":error}}})
    }

    #[test]
    fn dsh_collects_visible_work_and_excludes_internal_records() {
        let fixture = Fixture::new("visible");
        let now = now_ms();
        let rows = vec![
            message(1, now - 9_000, "今天完善导出功能", "user"),
            message(2, now - 8_500, "INJECTED_CONTEXT_MUST_NOT_APPEAR", "runtime-context"),
            message(3, now - 8_400, "SKILL_CATALOG_MUST_NOT_APPEAR", "skill-catalog"),
            message(4, now - 8_300, "队友协作消息", "agent-message"),
            assistant(5, now - 8_000, "PRIVATE_REASONING_MUST_NOT_APPEAR", "已调整中文列名", 1),
            tool_call(6, now - 7_500, "call-1", "pwsh", "{\"command\":\"npm test\"}"),
            tool_result(7, now - 7_000, "call-1", "测试通过", false),
            json!({"type":"system/message","seq":8,"time":now-6_000,
                "data":{"turn":1,"step":1,"message":{"role":"system","content":[{"type":"text","text":"SYSTEM_PROMPT_MUST_NOT_APPEAR"}],"source":{"kind":"system-prompt"},"id":"s1"}}}),
            json!({"type":"request/header","seq":9,"time":now-5_000,
                "data":{"header":{"config":{"provider":"test","model":"test"}}}}),
            json!({"type":"session/title","seq":10,"time":now-4_000,
                "data":{"title":"导出功能完善","source":{"kind":"user"}}}),
            json!({"type":"mystery/new-record","seq":11,"time":now-3_000,"data":{}}),
            json!({"type":"web/deepseek-search-llm-request","seq":12,"time":now-2_500,"data":{}}),
            message(13, now - 86_400_000, "YESTERDAY_MUST_NOT_APPEAR", "user"),
        ];
        fixture.write("session-visible", header("session-visible", now - 60_000, false), &rows, true);
        let result = fixture.collect();
        let capture = &result.capture;
        let events = capture["events"].as_array().unwrap();
        assert_eq!(events.len(), 5, "events: {events:?}");
        let kinds = events.iter().map(|event| text(event, "kind")).collect::<Vec<_>>();
        assert_eq!(kinds, ["user", "user", "assistant", "tool_call", "tool_result"]);
        let serialized = capture.to_string();
        for excluded in [
            "INJECTED_CONTEXT_MUST_NOT_APPEAR",
            "SKILL_CATALOG_MUST_NOT_APPEAR",
            "PRIVATE_REASONING_MUST_NOT_APPEAR",
            "SYSTEM_PROMPT_MUST_NOT_APPEAR",
            "YESTERDAY_MUST_NOT_APPEAR",
        ] {
            assert!(!serialized.contains(excluded), "unexpected captured text: {excluded}");
        }
        assert!(serialized.contains("协作消息 · 来源：其他代理"));
        assert!(serialized.contains("测试通过"));
        let session = &capture["sources"][0];
        assert_eq!(session["tool"], "dsh");
        assert_eq!(session["sessionCount"], 1);
        assert_eq!(session["eventCount"], 5);
        let sessions = &result.sessions;
        assert_eq!(sessions[0]["title"], "导出功能完善");
        assert_eq!(sessions[0]["project"], "C:\\Workspace\\Demo");
        assert_eq!(sessions[0]["tool"], "dsh");
        assert_eq!(sessions[0]["eventCount"], 5);
        assert!(capture["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("未采集的事件类型 mystery/new-record")));
        let ids = events
            .iter()
            .map(|event| text(event, "id").to_owned())
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), events.len());
        assert!(events.iter().all(|event| text(event, "tool") == "dsh"));
    }

    #[test]
    fn dsh_target_day_filter_keeps_only_that_day() {
        let fixture = Fixture::new("day");
        let now = now_ms();
        let yesterday = now - 86_400_000;
        let rows = vec![
            message(1, yesterday, "昨天的工作", "user"),
            message(2, now - 1_000, "今天的工作", "user"),
        ];
        fixture.write("session-day", header("session-day", yesterday, false), &rows, false);
        let capture = fixture.collect().capture;
        let events = capture["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(text(&events[0], "text"), "今天的工作");
    }

    #[test]
    fn dsh_seeded_session_skips_inherited_history() {
        let fixture = Fixture::new("seeded");
        let now = now_ms();
        let rows = vec![
            message(1, now - 8_000, "INHERITED_MUST_NOT_APPEAR", "user"),
            json!({"type":"session/end-seed","seq":2,"time":now-7_500,
                "data":{"inherited":true}}),
            message(3, now - 6_000, "NEW_WORK", "user"),
        ];
        let mut meta = header("session-seeded", now - 120_000, true);
        meta["parentSession"] = json!("session-parent");
        meta["origin"] = json!("subagent");
        meta["delegationDepth"] = json!(1);
        fixture.write("session-seeded", meta, &rows, true);
        let result = fixture.collect();
        let capture = &result.capture;
        let events = capture["events"].as_array().unwrap();
        assert_eq!(events.len(), 1, "events: {events:?}");
        assert_eq!(text(&events[0], "text"), "NEW_WORK");
        assert!(!capture.to_string().contains("INHERITED_MUST_NOT_APPEAR"));
        let session = &result.sessions[0];
        assert_eq!(session["parentSession"], "dsh:session-parent");
        assert_eq!(session["origin"], "subagent");
    }

    #[test]
    fn dsh_seeded_session_without_marker_is_not_reported_as_work() {
        let fixture = Fixture::new("seeded-missing");
        let now = now_ms();
        let rows = vec![message(1, now - 3_000, "MAYBE_INHERITED", "user")];
        fixture.write(
            "session-seeded-missing",
            header("session-seeded-missing", now - 60_000, true),
            &rows,
            false,
        );
        let result = fixture.collect();
        let capture = &result.capture;
        assert!(capture["events"].as_array().unwrap().is_empty());
        assert!(result.sessions.is_empty());
        assert!(capture["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("找不到继承截点")));
    }

    #[test]
    fn dsh_old_and_unknown_generations_are_skipped_with_warning() {
        let fixture = Fixture::new("versions");
        let now = now_ms();
        let dir = fixture.root.join("sessions/project");
        std::fs::create_dir_all(dir.join("old")).unwrap();
        std::fs::create_dir_all(dir.join("newer")).unwrap();
        std::fs::write(
            dir.join("old/session.v3.jsonl.zstd"),
            zstd::encode_all(serde_json::to_string(&header("old", now - 1000, false)).unwrap().as_bytes(), 3).unwrap(),
        ).unwrap();
        std::fs::write(
            dir.join("newer/session.v9.jsonl.zstd"),
            zstd::encode_all(serde_json::to_string(&header("newer", now - 1000, false)).unwrap().as_bytes(), 3).unwrap(),
        ).unwrap();
        let capture = fixture.collect().capture;
        let warnings = capture["warnings"].as_array().unwrap();
        assert!(warnings.iter().any(|warning| warning.as_str().unwrap().contains("旧版会话格式 v3")));
        assert!(warnings.iter().any(|warning| warning.as_str().unwrap().contains("较新会话格式 v9")));
        assert!(capture["events"].as_array().unwrap().is_empty());
    }

    #[test]
    fn dsh_torn_tail_keeps_complete_records() {
        let fixture = Fixture::new("torn");
        let now = now_ms();
        let path = fixture.write(
            "session-torn",
            header("session-torn", now - 60_000, false),
            &[message(1, now - 5_000, "完整记录", "user")],
            true,
        );
        // A frame whose first bytes decode the header but whose payload is truncated.
        let partial = zstd::encode_all(
            format!("{}\n", serde_json::to_string(&message(2, now - 4_000, "TORN_MUST_NOT_APPEAR", "user")).unwrap()).as_bytes(),
            3,
        ).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend(&partial[..partial.len() / 2]);
        std::fs::write(&path, bytes).unwrap();
        let capture = fixture.collect().capture;
        let events = capture["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(text(&events[0], "text"), "完整记录");
        assert!(capture["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("尾部记录仍在写入或已损坏")));
    }

    #[test]
    fn dsh_truncates_oversized_tool_output() {
        let fixture = Fixture::new("truncate");
        let now = now_ms();
        let body = "工具输出。".repeat(MAX_EVENT_CHARS);
        let rows = vec![
            message(1, now - 9_000, "执行命令", "user"),
            tool_call(2, now - 8_000, "call-1", "pwsh", "{\"command\":\"cat huge.log\"}"),
            tool_result(3, now - 7_000, "call-1", &body, false),
        ];
        fixture.write("session-truncate", header("session-truncate", now - 60_000, false), &rows, true);
        let capture = fixture.collect().capture;
        let events = capture["events"].as_array().unwrap();
        let result = events.iter().find(|event| text(event, "kind") == "tool_result").unwrap();
        let text = text(result, "text");
        assert!(text.contains("[已截断：原长"));
        assert!(text.chars().count() > MAX_EVENT_CHARS);
        assert!(text.chars().count() < MAX_EVENT_CHARS + 100);
    }
}
