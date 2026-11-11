use crate::common::{hash, redact, today, Result};
use anyhow::{bail, Context};
use chrono::{DateTime, NaiveDate, Timelike, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

pub fn default_root() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join(".codex")
        })
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn content(value: &Value) -> String {
    value
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| matches!(text(b, "type"), "input_text" | "output_text" | "text"))
                .map(|b| text(b, "text"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn visible_agent_body(value: &str) -> String {
    let normalized = value.replace("\r\n", "\n");
    let normalized = normalized.trim();
    if normalized.starts_with("Message Type:") {
        if let Some((_, body)) = normalized.split_once("\nPayload:") {
            return body.trim().to_owned();
        }
    }
    normalized.to_owned()
}

fn tool_output(payload: &Value) -> String {
    match payload.get("output") {
        None => String::new(),
        Some(Value::String(output)) => output.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| matches!(text(b, "type"), "input_text" | "output_text" | "text"))
            .map(|b| text(b, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => "[非文本工具结果未采集]".to_owned(),
    }
}

fn web_search(payload: &Value) -> Option<String> {
    let action = payload.get("action")?.as_object()?;
    if payload
        .get("status")
        .is_some_and(|s| !s.is_string() && !s.is_null())
    {
        return None;
    }
    let kind = action.get("type")?.as_str()?;
    let status = text(payload, "status");
    if !matches!(kind, "search" | "open_page" | "find_in_page")
        || !matches!(
            status,
            "" | "in_progress" | "searching" | "completed" | "failed"
        )
    {
        return None;
    }
    let mut visible = json!({"type":kind,"status":status});
    let keys: &[&str] = match kind {
        "search" => &["query"],
        "open_page" => &["url"],
        _ => &["url", "pattern"],
    };
    for key in keys {
        if let Some(value) = action.get(*key).filter(|v| !v.is_null()) {
            visible[*key] = json!(redact(value.as_str()?));
        }
    }
    if kind == "search" {
        if let Some(queries) = action.get("queries").filter(|v| !v.is_null()) {
            let mut selected = Vec::new();
            for query in queries.as_array()? {
                selected.push(redact(query.as_str()?));
            }
            visible["queries"] = json!(selected);
        }
    }
    Some(format!("web_search\n{}\n仅记录联网检索动作及其状态；此项不包含网页正文，也不证明相关工作已完成或验证。", visible))
}

pub struct SelectionCapture {
    pub capture: Value,
    pub sessions: Vec<Value>,
    pub global_warnings: Vec<String>,
}

pub fn collect(root: &Path, day: &str, zone: &str) -> Result<Value> {
    Ok(collect_for_selection(root, day, zone)?.capture)
}

pub fn collect_for_selection(root: &Path, day: &str, zone: &str) -> Result<SelectionCapture> {
    let target_day =
        NaiveDate::parse_from_str(day, "%Y-%m-%d").context("日期格式应为 YYYY-MM-DD")?;
    let timezone: Tz = crate::common::timezone(zone).context("部门时区无效")?;
    if day > today(zone)?.as_str() {
        bail!("不能提交未来日期");
    }
    let cutoff = Utc::now();
    let captured_at = cutoff.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    let mut events: Vec<Value> = Vec::new();
    let mut warnings = Vec::<String>::new();
    let mut session_info = HashMap::<String, Value>::new();
    let titles = session_titles(root);
    let mut seen = HashSet::new();
    let mut found_root = false;

    for directory in ["sessions", "archived_sessions"] {
        let folder = root.join(directory);
        if !folder.is_dir() {
            continue;
        }
        found_root = true;
        for entry in WalkDir::new(&folder).follow_links(false) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    warnings.push(format!("无法扫描 {directory} 中的部分目录，采集不完整"));
                    continue;
                }
            };
            if !entry.file_type().is_file()
                || entry.path().extension().and_then(|x| x.to_str()) != Some("jsonl")
            {
                continue;
            }
            let file = entry.path();
            let source = file
                .strip_prefix(root)
                .unwrap_or(file)
                .to_string_lossy()
                .to_string();
            let mut session = file
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let mut local_warnings = Vec::<String>::new();
            let mut session_title = String::new();
            let mut project = String::new();
            let stream = match File::open(file) {
                Ok(stream) => stream,
                Err(_) => {
                    session_info.insert(session.clone(), json!({"id":session,"title":"","project":"",
                        "warnings":[format!("无法读取 {}，采集不完整", redact(&source))]}));
                    continue;
                }
            };
            let mut primary: Vec<Value> = Vec::new();
            let mut fallback: Vec<Value> = Vec::new();
            let mut agent_bodies = HashMap::<String, String>::new();
            let mut malformed = false;
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
                let row: Value = match serde_json::from_str(line.trim_start_matches('\u{feff}')) {
                    Ok(Value::Object(row)) => Value::Object(row),
                    _ => {
                        malformed = true;
                        continue;
                    }
                };
                let row_type = text(&row, "type");
                let payload = match row.get("payload").filter(|p| p.is_object()) {
                    Some(payload) => payload,
                    None => {
                        if matches!(row_type, "response_item" | "event_msg") {
                            malformed = true;
                        }
                        continue;
                    }
                };
                if row_type == "session_meta" {
                    if !text(payload, "id").is_empty() {
                        session = text(payload, "id").to_owned();
                    }
                    project = redact(text(payload, "cwd"));
                    session_title = redact(if !text(payload, "title").is_empty() { text(payload, "title") } else { text(payload, "thread_name") });
                    continue;
                }
                let timestamp = match DateTime::parse_from_rfc3339(text(&row, "timestamp")) {
                    Ok(timestamp) => timestamp,
                    Err(_) => {
                        if matches!(row_type, "response_item" | "event_msg") {
                            malformed = true;
                        }
                        continue;
                    }
                };
                if timestamp.with_timezone(&timezone).date_naive() != target_day
                    || timestamp > cutoff
                {
                    continue;
                }
                let payload_type = text(payload, "type");
                let mut agent_body = None;
                let (kind, body) = if row_type == "response_item" {
                    match payload_type {
                        "message" => {
                            let role = text(payload, "role");
                            if !matches!(role, "user" | "assistant")
                                || (role == "assistant"
                                    && !matches!(
                                        text(payload, "channel"),
                                        "" | "final" | "commentary"
                                    ))
                            {
                                continue;
                            }
                            if !payload
                                .get("content")
                                .and_then(Value::as_array)
                                .is_some_and(|b| {
                                    b.iter()
                                        .all(|v| v.is_object() && !text(v, "type").is_empty())
                                })
                            {
                                malformed = true;
                            }
                            (role, content(payload))
                        }
                        "agent_message" => {
                            if !payload.get("content").is_some_and(Value::is_array) {
                                malformed = true;
                                continue;
                            }
                            let visible = visible_agent_body(&content(payload));
                            if visible.is_empty() {
                                continue;
                            }
                            let output = format!(
                                "协作消息 · 作者：{} · 接收方：{}\n{}",
                                text(payload, "author"),
                                text(payload, "recipient"),
                                visible
                            );
                            agent_body = Some(visible);
                            ("assistant", output)
                        }
                        "function_call" | "custom_tool_call" => (
                            "tool_call",
                            format!(
                                "{}\n{}",
                                text(payload, "name"),
                                text(
                                    payload,
                                    if payload_type == "function_call" {
                                        "arguments"
                                    } else {
                                        "input"
                                    }
                                )
                            ),
                        ),
                        "function_call_output" | "custom_tool_call_output" => {
                            ("tool_result", tool_output(payload))
                        }
                        "web_search_call" => match web_search(payload) {
                            Some(output) => ("tool_call", output),
                            None => {
                                local_warnings.push(format!("{source}:{line_number} 存在不支持或损坏的联网搜索动作，未采集该项"));
                                continue;
                            }
                        },
                        "reasoning" => continue,
                        other => {
                            local_warnings.push(format!(
                                "{source}:{line_number} 存在不支持的响应类型 {other}，未采集该项"
                            ));
                            continue;
                        }
                    }
                } else if row_type == "event_msg"
                    && matches!(payload_type, "user_message" | "agent_message")
                {
                    if payload_type == "agent_message"
                        && !matches!(text(payload, "phase"), "" | "final_answer" | "commentary")
                    {
                        continue;
                    }
                    (
                        if payload_type == "user_message" {
                            "user"
                        } else {
                            "assistant"
                        },
                        text(payload, "message").to_owned(),
                    )
                } else {
                    continue;
                };
                if body.trim().is_empty() {
                    continue;
                }
                // Match DateTimeOffset's O format so existing event references remain stable.
                let dotnet_time = format!(
                    "{}.{:07}{}",
                    timestamp.format("%Y-%m-%dT%H:%M:%S"),
                    timestamp.nanosecond() / 100,
                    timestamp.format("%:z")
                );
                let evidence_id = format!(
                    "E{}",
                    &hash(&format!("{session}:{line_number}:{dotnet_time}"))[..20]
                );
                let evidence = json!({"id":evidence_id,"sessionId":session,"timestamp":timestamp.to_rfc3339(),
                    "kind":kind,"text":redact(&body),"source":format!("{source}:{line_number}")});
                if let Some(visible) = agent_body {
                    agent_bodies.insert(evidence_id, redact(&visible));
                }
                if row_type == "response_item" {
                    primary.push(evidence);
                } else {
                    fallback.push(evidence);
                }
            }
            let duplicates = |item: &Value| {
                primary.iter().any(|p| {
                    p["kind"] == item["kind"]
                        && (p["text"] == item["text"]
                            || agent_bodies
                                .get(text(p, "id"))
                                .is_some_and(|b| b == &visible_agent_body(text(item, "text"))))
                })
            };
            let extra: Vec<_> = fallback
                .into_iter()
                .filter(|item| !duplicates(item))
                .collect();
            for item in primary.into_iter().chain(extra) {
                if seen.insert(text(&item, "id").to_owned()) {
                    events.push(item);
                }
            }
            if malformed {
                local_warnings.push(format!(
                    "{source} 含损坏、未写完或无时间戳的记录；无法保证完整，稍后可重交"
                ));
            }
            let info = session_info.entry(session.clone()).or_insert_with(|| json!({
                "id":session,"title":"","project":"","warnings":[]
            }));
            if !session_title.is_empty() { info["title"] = json!(session_title); }
            if !project.is_empty() { info["project"] = json!(project); }
            let attached = info["warnings"].as_array_mut().unwrap();
            for warning in local_warnings { let warning = json!(redact(&warning)); if !attached.contains(&warning) { attached.push(warning); } }
        }
    }
    if !found_root {
        warnings.push("未找到 Codex sessions 或 archived_sessions 目录，请检查记录路径".to_owned());
    }
    warnings.push("仅覆盖本机可读取的当日事件；跨日上下文、附件及外部文件未采集。".to_owned());
    let global_warnings = warnings.clone();
    for info in session_info.values() { warnings.extend(info["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).map(str::to_owned)); }
    if events.is_empty() { warnings.push("未发现可用工作记录；不代表当天没有工作".to_owned()); }
    let mut unique = HashSet::new();
    warnings.retain(|warning| unique.insert(warning.clone()));
    events.sort_by_cached_key(|event| {
        DateTime::parse_from_rfc3339(text(event, "timestamp"))
            .unwrap()
            .timestamp_micros()
    });
    let session_count = events
        .iter()
        .map(|e| text(e, "sessionId"))
        .collect::<HashSet<_>>()
        .len();
    let mut by_session = HashMap::<String, Vec<&Value>>::new();
    for event in &events { by_session.entry(text(event,"sessionId").to_owned()).or_default().push(event); }
    let mut sessions = Vec::new();
    for (sid, mut info) in session_info {
        let records = by_session.get(&sid).cloned().unwrap_or_default();
        if records.is_empty() && info["warnings"].as_array().unwrap().is_empty() { continue; }
        let title = titles.get(&sid).filter(|s| !s.is_empty()).cloned()
            .or_else(|| (!text(&info,"title").is_empty()).then(||text(&info,"title").to_owned()))
            .unwrap_or_else(|| records.iter().find(|e|text(e,"kind")=="user").map(|e|text(e,"text").split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default());
        info["title"] = json!(if title.is_empty() { format!("会话 {}",sid.chars().take(12).collect::<String>()) } else {title.chars().take(100).collect::<String>()});
        info["firstAt"] = records.first().map(|e|e["timestamp"].clone()).unwrap_or(Value::Null);
        info["lastAt"] = records.last().map(|e|e["timestamp"].clone()).unwrap_or(Value::Null);
        info["eventCount"] = json!(records.len());
        sessions.push(info);
    }
    sessions.sort_by(|a,b| text(b,"lastAt").cmp(text(a,"lastAt")).then(text(a,"id").cmp(text(b,"id"))));
    Ok(SelectionCapture {
        capture: json!({"day":day,"timeZone":zone,"capturedAt":captured_at,"events":events,
            "warnings":warnings,"sessionCount":session_count,"protocol":1}),
        sessions, global_warnings,
    })
}

fn session_titles(root: &Path) -> HashMap<String, String> {
    let mut titles = HashMap::new();
    if let Ok(file) = File::open(root.join("session_index.jsonl")) {
        for line in BufReader::new(file).lines().map_while(std::result::Result::ok) {
            if let Ok(row) = serde_json::from_str::<Value>(&line) {
                let sid = if text(&row,"id").is_empty() { text(&row,"thread_id") } else { text(&row,"id") };
                let title = text(&row,"thread_name");
                if !sid.is_empty() && !title.is_empty() { titles.insert(sid.to_owned(), redact(title)); }
            }
        }
    }
    titles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrated_collector_keeps_visible_cross_day_and_archived_records() -> Result<()> {
        let root =
            std::env::temp_dir().join(format!("daily-report-collector-{}", crate::common::id()));
        std::fs::create_dir_all(root.join("sessions/old"))?;
        std::fs::create_dir_all(root.join("archived_sessions"))?;
        let lines = [
            json!({"type":"session_meta","payload":{"id":"old-session"}}),
            json!({"timestamp":"2025-11-07T16:01:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"处理今天任务 sk-abcdefghijklmnopqrst"}]}}),
            json!({"timestamp":"2025-11-07T16:02:00Z","type":"response_item","payload":{"type":"message","role":"assistant","channel":"analysis","content":[{"type":"output_text","text":"PRIVATE_REASONING"}]}}),
            json!({"timestamp":"2025-11-07T16:03:00Z","type":"response_item","payload":{"type":"agent_message","author":"worker","recipient":"root","content":[{"type":"text","text":"协作结果"}]}}),
            json!({"timestamp":"2025-11-07T16:03:00Z","type":"event_msg","payload":{"type":"agent_message","message":"Message Type: FINAL_ANSWER\nPayload:\n协作结果"}}),
            json!({"timestamp":"2025-11-07T16:04:00Z","type":"response_item","payload":{"type":"agent_message","content":[{"type":"encrypted_text","text":"ENCRYPTED_BODY"}]}}),
            json!({"timestamp":"2025-11-07T15:59:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"YESTERDAY"}]}}),
        ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
        std::fs::write(root.join("sessions/old/old-created-date.jsonl"), lines)?;
        let archived = json!({"timestamp":"2025-11-08T04:00:00Z","type":"response_item","payload":{"type":"function_call_output","output":"验证完成"}});
        std::fs::write(
            root.join("archived_sessions/archived.jsonl"),
            format!("{archived}\n{{\"timestamp\":"),
        )?;
        let result = collect(&root, "2025-11-08", "Asia/Shanghai");
        // This directory is unique to this fixture and was created immediately above.
        std::fs::remove_dir_all(&root)?;
        let result = result?;
        assert_eq!(result["events"].as_array().unwrap().len(), 3);
        assert_eq!(result["sessionCount"], 2);
        let serialized = result.to_string();
        for excluded in [
            "PRIVATE_REASONING",
            "ENCRYPTED_BODY",
            "YESTERDAY",
            "sk-abcdefghijklmnopqrst",
        ] {
            assert!(
                !serialized.contains(excluded),
                "unexpected captured text: {excluded}"
            );
        }
        assert!(serialized.contains("验证完成"));
        assert!(result["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("未写完")));
        assert!(!result["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("不支持")));
        Ok(())
    }
}
