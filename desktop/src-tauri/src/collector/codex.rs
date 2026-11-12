use super::{block_text, local_day_start, text, truncate, LABEL_CODEX};
use anyhow::Result;
use chrono::{DateTime, NaiveDate, Timelike, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};
use walkdir::WalkDir;

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
        Some(Value::Array(blocks)) => block_text(Some(&Value::Array(blocks.clone()))),
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
            visible[*key] = json!(crate::common::redact(value.as_str()?));
        }
    }
    if kind == "search" {
        if let Some(queries) = action.get("queries").filter(|v| !v.is_null()) {
            let mut selected = Vec::new();
            for query in queries.as_array()? {
                selected.push(crate::common::redact(query.as_str()?));
            }
            visible["queries"] = json!(selected);
        }
    }
    Some(format!("web_search\n{}\n仅记录联网检索动作及其状态；此项不包含网页正文，也不证明相关工作已完成或验证。", visible))
}

fn session_titles(root: &Path) -> HashMap<String, String> {
    let mut titles = HashMap::new();
    if let Ok(file) = File::open(root.join("session_index.jsonl")) {
        for line in BufReader::new(file).lines().map_while(std::result::Result::ok) {
            if let Ok(row) = serde_json::from_str::<Value>(&line) {
                let sid = if text(&row, "id").is_empty() {
                    text(&row, "thread_id")
                } else {
                    text(&row, "id")
                };
                let title = text(&row, "thread_name");
                if !sid.is_empty() && !title.is_empty() {
                    titles.insert(sid.to_owned(), crate::common::redact(title));
                }
            }
        }
    }
    titles
}

/// Read today's visible Codex events and sessions below one Codex home.
pub(crate) fn collect(
    root: &Path,
    target_day: NaiveDate,
    timezone: Tz,
    cutoff: DateTime<Utc>,
) -> Result<super::SourceCapture> {
    let mut events: Vec<Value> = Vec::new();
    let mut warnings = Vec::<String>::new();
    let mut session_info = HashMap::<String, Value>::new();
    let titles = session_titles(root);
    let mut seen = HashSet::new();
    let mut found_root = false;
    let day_start = local_day_start(target_day, timezone);

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
                    warnings.push(format!("无法扫描 Codex {directory} 中的部分目录，采集不完整"));
                    continue;
                }
            };
            if !entry.file_type().is_file()
                || entry.path().extension().and_then(|x| x.to_str()) != Some("jsonl")
            {
                continue;
            }
            let file = entry.path();
            // A log last written before the target day cannot contain that day's events;
            // skip it before parsing the whole transcript. Keep one hour of slack for
            // clock changes while tailing an active session.
            if let Some(start) = day_start {
                if let Ok(modified) = std::fs::metadata(file).and_then(|metadata| metadata.modified()) {
                    let modified: DateTime<Utc> = modified.into();
                    if modified + chrono::Duration::hours(1) < start {
                        continue;
                    }
                }
            }
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
                        "warnings":[format!("无法读取 {}，采集不完整", crate::common::redact(&source))]}));
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
                    project = crate::common::redact(text(payload, "cwd"));
                    session_title = crate::common::redact(if !text(payload, "title").is_empty() {
                        text(payload, "title")
                    } else {
                        text(payload, "thread_name")
                    });
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
                    || timestamp.with_timezone(&Utc) > cutoff
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
                            (role, block_text(payload.get("content")))
                        }
                        "agent_message" => {
                            if !payload.get("content").is_some_and(Value::is_array) {
                                malformed = true;
                                continue;
                            }
                            let visible = visible_agent_body(&block_text(payload.get("content")));
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
                    &crate::common::hash(&format!("{session}:{line_number}:{dotnet_time}"))[..20]
                );
                let evidence = json!({"id":evidence_id,"sessionId":session,"timestamp":timestamp.to_rfc3339(),
                    "kind":kind,"text":crate::common::redact(&body),"source":format!("{source}:{line_number}"),
                    "tool":"codex"});
                if let Some(visible) = agent_body {
                    agent_bodies.insert(evidence_id, crate::common::redact(&visible));
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
            for mut item in primary.into_iter().chain(extra) {
                item["text"] = json!(truncate(text(&item, "text").to_owned()));
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
                "id":session,"title":"","project":"","warnings":[],"tool":"codex","toolLabel":LABEL_CODEX
            }));
            if !session_title.is_empty() { info["title"] = json!(session_title); }
            if !project.is_empty() { info["project"] = json!(project); }
            let attached = info["warnings"].as_array_mut().unwrap();
            for warning in local_warnings { let warning = json!(crate::common::redact(&warning)); if !attached.contains(&warning) { attached.push(warning); } }
        }
    }
    if !found_root {
        warnings.push("未找到 Codex sessions 或 archived_sessions 目录，请检查记录路径".to_owned());
    }
    let mut by_session = HashMap::<String, Vec<&Value>>::new();
    for event in &events {
        by_session.entry(text(event, "sessionId").to_owned()).or_default().push(event);
    }
    let mut sessions = Vec::new();
    for (sid, mut info) in session_info {
        let records = by_session.get(&sid).cloned().unwrap_or_default();
        if records.is_empty() && info["warnings"].as_array().unwrap().is_empty() {
            continue;
        }
        let title = titles
            .get(&sid)
            .filter(|s| !s.is_empty())
            .cloned()
            .or_else(|| (!text(&info, "title").is_empty()).then(|| text(&info, "title").to_owned()))
            .unwrap_or_else(|| {
                records
                    .iter()
                    .find(|e| text(e, "kind") == "user")
                    .map(|e| text(e, "text").split_whitespace().collect::<Vec<_>>().join(" "))
                    .unwrap_or_default()
            });
        info["title"] = json!(if title.is_empty() {
            format!("会话 {}", sid.chars().take(12).collect::<String>())
        } else {
            title.chars().take(100).collect::<String>()
        });
        info["firstAt"] = records.first().map(|e| e["timestamp"].clone()).unwrap_or(Value::Null);
        info["lastAt"] = records.last().map(|e| e["timestamp"].clone()).unwrap_or(Value::Null);
        info["eventCount"] = json!(records.len());
        sessions.push(info);
    }
    Ok(super::SourceCapture { sessions, events, global_warnings: warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::{collect_for_selection, SourceRoots};

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
        let result = collect_for_selection(
            &SourceRoots { codex: Some(root.clone()), dsh: None, trae: None, trae_local: None },
            "2025-11-08",
            "Asia/Shanghai",
        );
        // This directory is unique to this fixture and was created immediately above.
        std::fs::remove_dir_all(&root)?;
        let result = result?;
        let capture = &result.capture;
        assert_eq!(capture["events"].as_array().unwrap().len(), 3);
        assert_eq!(capture["sessionCount"], 2);
        assert!(capture["sources"].as_array().unwrap().iter().any(|source| source["tool"] == "codex"));
        let serialized = capture.to_string();
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
        assert!(capture["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("未写完")));
        assert!(!capture["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("不支持")));
        Ok(())
    }

    #[test]
    fn migrated_collector_limits_single_event_size() -> Result<()> {
        let root = std::env::temp_dir().join(format!("daily-report-truncate-{}", crate::common::id()));
        std::fs::create_dir_all(root.join("sessions"))?;
        let day = crate::common::today("Asia/Shanghai")?;
        let body = "工具输出行。".repeat(super::super::MAX_EVENT_CHARS);
        let lines = [
            json!({"type":"session_meta","payload":{"id":"truncate-session"}}),
            json!({"timestamp":crate::common::now(),"type":"response_item","payload":{"type":"function_call_output","output":body}}),
        ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
        std::fs::write(root.join("sessions/truncate.jsonl"), lines)?;
        let result = collect_for_selection(&SourceRoots { codex: Some(root.clone()), dsh: None, trae: None, trae_local: None }, &day, "Asia/Shanghai");
        std::fs::remove_dir_all(&root)?;
        let capture = result?.capture;
        let text = capture["events"][0]["text"].as_str().unwrap();
        assert!(text.contains("[已截断：原长"));
        assert!(text.contains("仅保留前 200000 字符]"));
        let count = text.chars().count();
        assert!(
            count > super::super::MAX_EVENT_CHARS && count < super::super::MAX_EVENT_CHARS + 100,
            "unexpected truncated length: {count}"
        );
        Ok(())
    }
}
