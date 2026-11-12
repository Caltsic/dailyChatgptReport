use super::{local_day_start, truncate, LABEL_TRAE};
use anyhow::Result;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use regex::Regex;
use serde_json::{json, Value};
use std::{
    fs,
    path::Path,
    sync::LazyLock,
};
use walkdir::WalkDir;

const TOOL: &str = "trae";
/// Conversation exports are plain files; only these extensions are considered.
const SUPPORTED: &[&str] = &["md", "markdown", "txt", "json", "jsonl"];
/// One export above this size is skipped rather than buffered in memory.
const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;

struct Message {
    role: String,
    text: String,
    at: Option<DateTime<Utc>>,
    /// File line for Markdown/JSONL, message index for a JSON document.
    line: Option<usize>,
}

/// Read manually exported Trae conversations from one folder. Trae keeps its own
/// conversations in an encrypted database; this adapter only consumes what the user
/// exported (Markdown/JSON/JSONL) and states its assumptions in collection warnings.
pub(crate) fn collect(
    root: &Path,
    target_day: NaiveDate,
    timezone: Tz,
    cutoff: DateTime<Utc>,
) -> Result<super::SourceCapture> {
    let mut global_warnings = Vec::new();
    if !root.is_dir() {
        global_warnings.push(format!("{LABEL_TRAE}：导出目录不存在或不可读"));
        return Ok(super::SourceCapture {
            sessions: Vec::new(),
            events: Vec::new(),
            global_warnings,
        });
    }
    let mut files = Vec::new();
    for entry in WalkDir::new(root).max_depth(3).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                global_warnings.push(format!("{LABEL_TRAE}：无法扫描部分导出目录，采集不完整"));
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_owned();
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !SUPPORTED.contains(&extension.as_str()) {
            continue;
        }
        files.push((path, extension));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut sessions = Vec::new();
    let mut events = Vec::new();
    let mut unrecognized = 0usize;
    let mut untimed = 0usize;
    for (path, extension) in files {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        let size = fs::metadata(&path).map(|metadata| metadata.len()).unwrap_or(0);
        if size > MAX_FILE_BYTES {
            global_warnings.push(format!("{LABEL_TRAE}：{relative} 超过 20MB，未采集"));
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => {
                global_warnings.push(format!("{LABEL_TRAE}：无法读取 {relative}，采集不完整"));
                continue;
            }
        };
        let Some(file_day) = filename_day(&relative)
            .or_else(|| file_modified_day(&path, timezone))
        else {
            global_warnings.push(format!("{LABEL_TRAE}：{relative} 无法判断日期，未采集"));
            continue;
        };
        let modified = fs::metadata(&path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .map(DateTime::<Utc>::from);
        let (messages, mut session_warnings, parsed_title) =
            if extension == "json" || extension == "jsonl" {
                parse_json(&content, &extension, timezone)
            } else {
                let (messages, warnings) = parse_text(&content, timezone);
                (messages, warnings, None)
            };
        if messages.is_empty() {
            unrecognized += 1;
            session_warnings.push(format!(
                "{relative} 未识别到对话内容（{extension}），未采集"
            ));
            global_warnings.extend(session_warnings);
            continue;
        }
        let raw_id = &crate::common::hash(&relative)[..16];
        let session_id = format!("{TOOL}:{raw_id}");
        let mut local = Vec::new();
        let mut used_fallback_time = false;
        for (index, message) in messages.iter().enumerate() {
            let at = match message.at {
                Some(value) => {
                    if value.with_timezone(&timezone).date_naive() != target_day {
                        continue;
                    }
                    value
                }
                None => {
                    if file_day != target_day {
                        continue;
                    }
                    used_fallback_time = true;
                    fallback_instant(file_day, modified, timezone)
                }
            };
            if at > cutoff {
                continue;
            }
            let stamp = at
                .with_timezone(&timezone)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
            let ordinal = message.line.unwrap_or(index + 1);
            let evidence_id = format!(
                "E{}",
                &crate::common::hash(&format!("{session_id}:{ordinal}:{stamp}"))[..20]
            );
            if message.text.trim().is_empty() {
                continue;
            }
            local.push(json!({"id":evidence_id,"sessionId":session_id,"timestamp":stamp,
                "kind":message.role,"text":truncate(crate::common::redact(&message.text)),
                "source":format!("{relative}:{ordinal}"),"tool":TOOL}));
        }
        if local.is_empty() && session_warnings.is_empty() {
            continue;
        }
        if used_fallback_time {
            untimed += 1;
            session_warnings.push(format!("说明：{relative} 部分记录没有时间戳，按文件日期计入"));
        }
        let title = parsed_title.unwrap_or_else(|| session_title(&content, &relative));
        let session = json!({"id":session_id,"title":title,"project":"","warnings":session_warnings,
            "tool":TOOL,"toolLabel":LABEL_TRAE,
            "firstAt":local.first().map(|event| event["timestamp"].clone()).unwrap_or(Value::Null),
            "lastAt":local.last().map(|event| event["timestamp"].clone()).unwrap_or(Value::Null),
            "eventCount":local.len()});
        events.extend(local);
        sessions.push(session);
    }
    if unrecognized > 0 {
        global_warnings.push(format!("{LABEL_TRAE}：{unrecognized} 个导出文件未识别到对话内容"));
    }
    if untimed > 0 {
        global_warnings.push(format!(
            "说明：{LABEL_TRAE}：{untimed} 个导出文件缺少逐条时间戳，已按文件日期计入"
        ));
    }
    Ok(super::SourceCapture {
        sessions,
        events,
        global_warnings,
    })
}

fn fallback_instant(day: NaiveDate, modified: Option<DateTime<Utc>>, timezone: Tz) -> DateTime<Utc> {
    if let Some(modified) = modified {
        if modified.with_timezone(&timezone).date_naive() == day {
            return modified;
        }
    }
    local_day_start(day, timezone)
        .map(|start| start + chrono::Duration::hours(12))
        .unwrap_or_else(|| {
            let naive = NaiveDateTime::new(day, NaiveTime::from_hms_opt(12, 0, 0).unwrap());
            Utc.from_utc_datetime(&naive)
        })
}

fn file_modified_day(path: &Path, timezone: Tz) -> Option<NaiveDate> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let modified: DateTime<Utc> = modified.into();
    Some(modified.with_timezone(&timezone).date_naive())
}

fn filename_day(relative: &str) -> Option<NaiveDate> {
    static DATE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:.*[\\/])?(20\d{2})[-_.年]?(1[0-2]|0?[1-9])[-_.月]?(3[01]|[12]\d|0?[1-9])日?").unwrap()
    });
    let name = relative.rsplit(['\\', '/']).next().unwrap_or(relative);
    let captures = DATE.captures(name)?;
    let year = captures.get(1)?.as_str().parse().ok()?;
    let month = captures.get(2)?.as_str().parse().ok()?;
    let day = captures.get(3)?.as_str().parse().ok()?;
    NaiveDate::from_ymd_opt(year, month, day)
}

fn session_title(content: &str, relative: &str) -> String {
    for line in content.lines().take(40) {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            continue;
        }
        let heading = trimmed
            .trim_start_matches('#')
            .trim_matches(|character: char| character == '*' || character == '_' || character.is_whitespace());
        if heading.is_empty() || heading_role(heading).is_some() {
            continue;
        }
        return crate::common::redact(heading).chars().take(100).collect();
    }
    let stem = relative.rsplit(['\\', '/']).next().unwrap_or(relative);
    let stem = stem.rsplit_once('.').map(|(value, _)| value).unwrap_or(stem);
    let stem = stem
        .trim_start_matches(|character: char| character.is_ascii_digit() || matches!(character, '-' | '_' | '.'))
        .trim_start_matches(|character: char| character == '年' || character == '月' || character == '日')
        .trim_matches(|character: char| character.is_whitespace());
    let title = if stem.is_empty() { "Trae 导出会话".to_owned() } else { crate::common::redact(stem) };
    title.chars().take(100).collect()
}

/// Markdown/text role headings are matched strictly so a conversation title such as
/// "Trae 导出会话" is not mistaken for a role marker.
fn heading_role(raw: &str) -> Option<&'static str> {
    let trimmed = raw
        .trim()
        .trim_matches(|character: char| matches!(character, '*' | '_' | ' '));
    // Strip emoji/icon prefixes such as "👤 用户".
    let cleaned = trimmed
        .trim_start_matches(|character: char| {
            character.is_whitespace() || !(character.is_alphanumeric() || character as u32 > 0x2E80)
        })
        .trim();
    let value = cleaned
        .trim_end_matches(|character: char| {
            matches!(character, '：' | ':' | '（' | '(' | '）' | ')' | '-' | '—' | '|' | ' ' | '*' | '_')
        })
        .to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    for (keywords, kind) in [
        (&["工具调用", "调用工具", "tool call", "tool_call"][..], "tool_call"),
        (&["工具结果", "工具输出", "tool result", "tool_result", "tool output"][..], "tool_result"),
        (&["系统", "system", "developer", "指令"][..], "system"),
        (&["用户", "我", "提问", "user", "human", "prompt"][..], "user"),
        (&["助手", "回答", "回复", "assistant", "ai"][..], "assistant"),
    ] {
        for keyword in keywords {
            if value == *keyword {
                return Some(kind);
            }
            if let Some(rest) = value.strip_prefix(keyword) {
                if rest.chars().all(|character| {
                    character.is_whitespace()
                        || matches!(character, '：' | ':' | '（' | '(' | '）' | ')' | '-' | '—' | '|' | '*' | '_')
                }) {
                    return Some(kind);
                }
            }
        }
    }
    None
}

/// Role values from JSON exports are looser: they may use provider names as the author.
fn role_kind_loose(raw: &str) -> Option<&'static str> {
    let value = raw.trim().trim_matches(|character: char| character == '*' || character == '_').trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    for (keywords, kind) in [
        (&["工具调用", "调用工具", "tool call", "tool_call", "function call", "function_call"][..], "tool_call"),
        (&["工具结果", "工具输出", "tool result", "tool_result", "tool output", "tool_output"][..], "tool_result"),
        (&["系统", "system", "developer", "instruction"][..], "system"),
        (&["用户", "我", "提问", "user", "human", "prompt"][..], "user"),
        (&["助手", "回答", "回复", "assistant", "ai", "trae", "agent", "model", "bot"][..], "assistant"),
    ] {
        if keywords.iter().any(|keyword| value.starts_with(keyword)) {
            return Some(kind);
        }
    }
    None
}

fn role_marker(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    let (heading, inline_rest) = if let Some(rest) = trimmed.strip_prefix('#') {
        (rest.trim_start_matches('#').trim(), None)
    } else if trimmed.starts_with("**") && trimmed[2..].contains("**") {
        let end = trimmed[2..].find("**")? + 2;
        let heading = trimmed[2..end].trim();
        let after = trimmed[end + 2..]
            .trim_start_matches(|character: char| matches!(character, '：' | ':' | ' '));
        (heading, Some(after.to_owned()))
    } else {
        let (heading, after) = trimmed.split_once(['：', ':'])?;
        (heading.trim(), Some(after.trim().to_owned()))
    };
    let candidate = heading
        .trim()
        .trim_matches(|character: char| matches!(character, '*' | '_' | '#' | ' '))
        .trim_end_matches(['：', ':'])
        .trim();
    if candidate.is_empty() || candidate.chars().count() > 24 {
        return None;
    }
    let kind = heading_role(candidate)?;
    Some((kind.to_owned(), inline_rest.unwrap_or_default()))
}

fn timestamp_in_utc(line: &str, timezone: Tz) -> Option<DateTime<Utc>> {
    static STAMP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(20\d{2})[-/](\d{1,2})[-/](\d{1,2})[ T](\d{1,2}):(\d{2})(?::(\d{2}))?").unwrap()
    });
    let captures = STAMP.captures(line)?;
    let mut numbers = (1..=6).map(|index| {
        captures
            .get(index)
            .and_then(|value| value.as_str().parse::<u32>().ok())
            .unwrap_or(0)
    });
    let date = NaiveDate::from_ymd_opt(
        numbers.next()? as i32,
        numbers.next()?,
        numbers.next()?,
    )?;
    let time = NaiveTime::from_hms_opt(numbers.next()?, numbers.next()?, numbers.next()?)?;
    let naive = date.and_time(time);
    timezone
        .from_local_datetime(&naive)
        .earliest()
        .map(|value| value.with_timezone(&Utc))
}

fn parse_text(content: &str, timezone: Tz) -> (Vec<Message>, Vec<String>) {
    let mut messages = Vec::new();
    let warnings = Vec::new();
    let mut current: Option<(String, Option<DateTime<Utc>>, usize, String)> = None;
    let mut preamble = String::new();
    for (index, line) in content.lines().enumerate() {
        let line_number = index + 1;
        if let Some((role, inline)) = role_marker(line) {
            if let Some((role, at, start, body)) = current.take() {
                push_section(&mut messages, role, at, start, body);
            }
            current = Some((role, timestamp_in_utc(line, timezone), line_number, inline));
        } else if let Some((_, _, _, body)) = current.as_mut() {
            body.push('\n');
            body.push_str(line);
        } else {
            preamble.push('\n');
            preamble.push_str(line);
        }
    }
    if let Some((role, at, start, body)) = current.take() {
        push_section(&mut messages, role, at, start, body);
    }
    if messages.is_empty() {
        let cleaned = strip_metadata(content);
        if !cleaned.trim().is_empty() {
            messages.push(Message {
                role: "assistant".to_owned(),
                text: cleaned,
                at: timestamp_in_utc(content, timezone),
                line: Some(1),
            });
        }
    } else {
        let preamble = strip_preamble(&preamble);
        if !preamble.trim().is_empty() {
            // Content before the first role marker is kept as context, so work text that
            // appears before any role heading is not silently dropped.
            messages.insert(
                0,
                Message {
                    role: "assistant".to_owned(),
                    text: preamble,
                    at: None,
                    line: Some(1),
                },
            );
        }
    }
    (messages, warnings)
}

fn push_section(messages: &mut Vec<Message>, role: String, at: Option<DateTime<Utc>>, start: usize, body: String) {
    if role == "system" {
        return;
    }
    let text = body.trim_matches('\n').trim();
    if text.is_empty() {
        return;
    }
    messages.push(Message {
        role,
        text: text.to_owned(),
        at,
        line: Some(start),
    });
}

fn strip_metadata(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !is_metadata_line(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Text before the first role marker is usually an export header (title, export time);
/// keep only lines that are neither metadata nor Markdown headings.
fn strip_preamble(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#') && !is_metadata_line(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_metadata_line(line: &str) -> bool {
    static META: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*(?:导出|导出时间|生成时间|会话标题|会话|标题|时间|Title|Export|Exported|Generated|Session|Time)\s*[:：]").unwrap()
    });
    META.is_match(line)
}

fn parse_json(
    content: &str,
    extension: &str,
    timezone: Tz,
) -> (Vec<Message>, Vec<String>, Option<String>) {
    let mut warnings = Vec::new();
    let mut title = None;
    if extension == "jsonl" {
        let mut messages = Vec::new();
        for (index, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(line) {
                Ok(value) => collect_messages(
                    &value,
                    &mut messages,
                    &mut warnings,
                    &mut title,
                    timezone,
                    None,
                    Some(index + 1),
                ),
                Err(_) => warnings.push(format!("第 {} 行不是有效 JSON，已跳过", index + 1)),
            }
        }
        return (messages, warnings, title);
    }
    match serde_json::from_str::<Value>(content) {
        Ok(value) => {
            let mut messages = Vec::new();
            collect_messages(
                &value,
                &mut messages,
                &mut warnings,
                &mut title,
                timezone,
                None,
                None,
            );
            (messages, warnings, title)
        }
        Err(_) => {
            // Some exports use a .json name but write one JSON object per line.
            let mut messages = Vec::new();
            for (index, line) in content.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(line) {
                    collect_messages(
                        &value,
                        &mut messages,
                        &mut warnings,
                        &mut title,
                        timezone,
                        None,
                        Some(index + 1),
                    );
                }
            }
            if messages.is_empty() {
                warnings.push("文件不是有效 JSON 或 JSONL，未采集".to_owned());
            }
            (messages, warnings, title)
        }
    }
}

/// Timestamp keys used by Trae message rows and by wrapper records such as `history_v2`.
const TIMESTAMP_KEYS: [&str; 10] = [
    "timestamp", "time", "createdAt", "created_at", "date", "sendTime", "send_time", "ts",
    "pubTime", "create_time",
];
const TITLE_KEYS: [&str; 5] = [
    "session_title", "sessionTitle", "conversation_title", "conversationTitle", "title",
];
const MESSAGE_CONTAINER_KEYS: [&str; 22] = [
    "messages", "raw_messages", "rawMessages", "conversation", "conversations", "chat", "chats",
    "history", "history_v2", "historyV2", "chat_messages", "chatMessages", "records", "items",
    "turns", "data", "log", "logs", "content", "children", "sessions", "session",
];

fn collect_messages(
    value: &Value,
    messages: &mut Vec<Message>,
    warnings: &mut Vec<String>,
    title: &mut Option<String>,
    timezone: Tz,
    inherited: Option<DateTime<Utc>>,
    line: Option<usize>,
) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_messages(item, messages, warnings, title, timezone, inherited, line);
            }
        }
        Value::Object(object) => {
            if let Some(mut message) = message_from_object(object, timezone, line) {
                // A record without its own timestamp inherits the timestamp of the
                // session/export wrapper it came from (e.g. Trae `history_v2.created_at`).
                message.at = message.at.or(inherited);
                messages.push(message);
                return;
            }
            if title.is_none() {
                if let Some(value) = TITLE_KEYS
                    .iter()
                    .find_map(|key| object.get(*key).and_then(Value::as_str))
                {
                    let clean = value.trim();
                    if !clean.is_empty() {
                        *title =
                            Some(crate::common::redact(clean).chars().take(100).collect());
                    }
                }
            }
            let next_inherited = object_timestamp(object, timezone).or(inherited);
            for key in MESSAGE_CONTAINER_KEYS {
                if let Some(child) = object.get(key) {
                    if child.is_array() || child.is_object() {
                        collect_messages(
                            child,
                            messages,
                            warnings,
                            title,
                            timezone,
                            next_inherited,
                            line,
                        );
                    }
                }
            }
        }
        _ => {}
    }
}

fn object_timestamp(
    object: &serde_json::Map<String, Value>,
    timezone: Tz,
) -> Option<DateTime<Utc>> {
    TIMESTAMP_KEYS
        .iter()
        .find_map(|key| object.get(*key).and_then(|value| value_as_timestamp(value, timezone)))
}

fn message_from_object(
    object: &serde_json::Map<String, Value>,
    timezone: Tz,
    line: Option<usize>,
) -> Option<Message> {
    let role_raw = ["role", "author", "sender", "speaker", "from", "type"]
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))?;
    let kind = role_kind_loose(role_raw)?;
    if kind == "system" {
        return None;
    }
    let body = ["content", "text", "message", "body", "value"]
        .iter()
        .find_map(|key| object.get(*key).and_then(text_from_value))?;
    if body.trim().is_empty() {
        return None;
    }
    Some(Message {
        role: kind.to_owned(),
        text: body,
        at: object_timestamp(object, timezone),
        line,
    })
}

/// Blocks that are internal reasoning rather than visible conversation content.
const SKIP_BLOCK_TYPES: [&str; 5] = ["reasoning", "thinking", "internal", "redacted_thinking", "analysis"];

fn text_from_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(text_from_value)
                .filter(|part| !part.trim().is_empty())
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(object) => {
            if object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| SKIP_BLOCK_TYPES.contains(&kind))
            {
                return None;
            }
            for key in ["text", "content", "message", "body"] {
                if let Some(text) = object.get(key).and_then(text_from_value) {
                    if !text.trim().is_empty() {
                        return Some(text);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn value_as_timestamp(value: &Value, timezone: Tz) -> Option<DateTime<Utc>> {
    if let Some(number) = value.as_i64() {
        let millis = if number.abs() > 1_000_000_000_000 { number } else { number * 1000 };
        return DateTime::<Utc>::from_timestamp_millis(millis);
    }
    let text = value.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(text) {
        return Some(parsed.with_timezone(&Utc));
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S", "%Y/%m/%d %H:%M:%S"] {
        if let Ok(parsed) = NaiveDateTime::parse_from_str(text, format) {
            return timezone
                .from_local_datetime(&parsed)
                .earliest()
                .map(|value| value.with_timezone(&Utc));
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return local_day_start(date, timezone);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::{collect_for_selection, SourceRoots};

    struct Fixture {
        root: std::path::PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "daily-report-trae-{tag}-{}",
                crate::common::id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }
        fn write(&self, name: &str, content: &str) {
            if let Some(parent) = self.root.join(name).parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(self.root.join(name), content).unwrap();
        }
        fn day(&self) -> String {
            crate::common::today("Asia/Shanghai").unwrap()
        }
        fn collect(&self) -> crate::collector::SelectionCapture {
            collect_for_selection(
                &SourceRoots {
                    codex: None,
                    dsh: None,
                    trae: Some(self.root.clone()),
                    trae_local: None,
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

    #[test]
    fn trae_markdown_export_keeps_roles_and_redacts() {
        let fixture = Fixture::new("markdown");
        let day = fixture.day();
        fixture.write(
            &format!("{day}-导出会话.md"),
            &format!(
                "# 导出会话\n导出时间：{day} 20:00:00\n## 用户\n帮我整理导出功能 2026-11-12 10:20:00\n## 助手\n已完成列名调整，密钥 sk-abcdefghijklmnopqrst 已轮换\n## 系统\nINTERNAL_SYSTEM_MUST_NOT_APPEAR\n",
            ),
        );
        let result = fixture.collect();
        let capture = &result.capture;
        let events = capture["events"].as_array().unwrap();
        assert_eq!(events.len(), 2, "events: {events:?}");
        assert_eq!(events[0]["kind"], "user");
        assert_eq!(events[1]["kind"], "assistant");
        let serialized = capture.to_string();
        assert!(!serialized.contains("sk-abcdefghijklmnopqrst"));
        assert!(!serialized.contains("INTERNAL_SYSTEM_MUST_NOT_APPEAR"));
        assert!(serialized.contains("已完成列名调整"));
        assert_eq!(result.sessions[0]["tool"], "trae");
        assert_eq!(result.sessions[0]["title"], "导出会话");
        assert_eq!(result.sessions[0]["eventCount"], 2);
        assert!(capture["sources"].as_array().unwrap().iter().any(|source| source["tool"] == "trae"));
    }

    #[test]
    fn trae_json_export_reads_messages_and_skips_system() {
        let fixture = Fixture::new("json");
        let day = fixture.day();
        fixture.write(
            &format!("{day}.json"),
            &format!(
                r#"{{"title":"Trae JSON 会话","messages":[
                    {{"role":"user","content":"整理接口超时","time":"{day}T09:10:00+08:00"}},
                    {{"role":"assistant","content":[{{"type":"text","text":"已定位下游等待"}}],"time":"{day}T09:12:00+08:00"}},
                    {{"role":"system","content":"INTERNAL_SYSTEM_MUST_NOT_APPEAR","time":"{day}T09:00:00+08:00"}}
                ]}}"#,
            ),
        );
        let capture = fixture.collect().capture;
        let events = capture["events"].as_array().unwrap();
        assert_eq!(events.len(), 2, "events: {events:?}");
        assert_eq!(events[0]["kind"], "user");
        assert_eq!(events[1]["kind"], "assistant");
        assert!(!capture.to_string().contains("INTERNAL_SYSTEM_MUST_NOT_APPEAR"));
        assert!(capture.to_string().contains("已定位下游等待"));
    }

    #[test]
    fn trae_jsonl_export_reads_lines() {
        let fixture = Fixture::new("jsonl");
        let day = fixture.day();
        let lines = [
            json!({"role":"user","content":"第一问","timestamp":format!("{day}T08:00:00+08:00")}),
            json!({"role":"assistant","content":"第一答","timestamp":format!("{day}T08:01:00+08:00")}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        fixture.write(&format!("{day}.jsonl"), &lines);
        let capture = fixture.collect().capture;
        assert_eq!(capture["events"].as_array().unwrap().len(), 2);
        assert_eq!(capture["events"][0]["kind"], "user");
    }

    #[test]
    fn trae_export_without_timestamps_uses_file_date() {
        let fixture = Fixture::new("untimed");
        let day = fixture.day();
        fixture.write(&format!("{day}-会话.md"), "用户：没有时间戳的问题\n助手：没有时间戳的回答\n");
        let capture = fixture.collect().capture;
        let events = capture["events"].as_array().unwrap();
        assert_eq!(events.len(), 2);
        assert!(capture["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("按文件日期计入")));
        assert!(events
            .iter()
            .all(|event| event["timestamp"].as_str().unwrap().starts_with(&day)));
    }

    #[test]
    fn trae_export_for_other_day_is_not_collected() {
        let fixture = Fixture::new("other-day");
        fixture.write("2025-11-07-旧会话.md", "用户：旧问题\n助手：旧回答\n");
        let capture = fixture.collect().capture;
        assert!(capture["events"].as_array().unwrap().is_empty());
    }

    #[test]
    fn trae_unrecognized_file_is_reported_and_skipped() {
        let fixture = Fixture::new("invalid");
        let day = fixture.day();
        fixture.write(&format!("{day}-坏文件.json"), "{not json");
        fixture.write(&format!("{day}-说明.txt"), "");
        let capture = fixture.collect().capture;
        assert!(capture["events"].as_array().unwrap().is_empty());
        assert!(capture["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("未识别到对话内容")));
    }

    #[test]
    fn trae_history_wrapper_imports_raw_messages_with_record_title_and_time() {
        let fixture = Fixture::new("history-wrapper");
        let day = fixture.day();
        let now = chrono::Utc::now().timestamp_millis();
        // Shape of Trae CN `history_v2.messages`: one record with a wrapper timestamp,
        // raw_messages and per-block text plus an internal reasoning block.
        fixture.write(
            &format!("{day}-history.json"),
            &format!(
                r#"{{"session_title":"CN 导出会话","created_at":{now},"messages":{{"raw_messages":[
                    {{"role":"user","content":[{{"type":"text","text":"WRAP_USER 整理导出"}}]}},
                    {{"role":"assistant","content":[{{"type":"text","text":"WRAP_ASSISTANT 已完成"}},{{"type":"reasoning","text":"WRAP_REASONING_MUST_NOT_APPEAR"}}]}}
                ]}}}}"#
            ),
        );
        let result = fixture.collect();
        let capture = &result.capture;
        let events = capture["events"].as_array().unwrap();
        assert_eq!(events.len(), 2, "events: {events:?}");
        assert_eq!(events[0]["kind"], "user");
        assert_eq!(events[1]["kind"], "assistant");
        assert!(events
            .iter()
            .all(|event| event["timestamp"].as_str().unwrap().starts_with(&day)));
        assert!(!capture.to_string().contains("WRAP_REASONING_MUST_NOT_APPEAR"));
        assert_eq!(result.sessions[0]["title"], "CN 导出会话");
    }

    #[test]
    fn trae_record_wrapper_inherits_timestamp_for_list_messages() {
        let fixture = Fixture::new("record-wrapper");
        let day = fixture.day();
        let now = chrono::Utc::now().timestamp_millis();
        // A list of per-session records, each with its own createdAt and flat messages.
        fixture.write(
            &format!("{day}-records.json"),
            &format!(
                r#"{{"sessions":[
                    {{"title":"记录一","createdAt":{now},"messages":[
                        {{"role":"user","text":"REC_USER 记录内容"}},
                        {{"role":"assistant","text":"REC_ASSISTANT 记录回复"}}
                    ]}}
                ]}}"#
            ),
        );
        let result = fixture.collect();
        let capture = &result.capture;
        let events = capture["events"].as_array().unwrap();
        assert_eq!(events.len(), 2, "events: {events:?}");
        assert!(events
            .iter()
            .all(|event| event["timestamp"].as_str().unwrap().starts_with(&day)));
        assert_eq!(result.sessions[0]["title"], "记录一");
    }
}
