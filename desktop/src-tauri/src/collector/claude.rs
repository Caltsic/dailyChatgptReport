use super::{local_day_start, text, truncate, LABEL_CLAUDE};
use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

const TOOL: &str = "claude";

struct RawRecord {
    line_number: usize,
    block_idx: usize,
    utc_ts: DateTime<Utc>,
    kind: String,
    body: String,
    extra: Option<serde_json::Map<String, Value>>,
}

const IGNORED_TYPES: &[&str] = &[
    "system",
    "attachment",
    "file-history-snapshot",
    "file-history-delta",
    "cost-state",
    "mode",
    "permission-mode",
    "queue-operation",
    "last-prompt",
    "atis-latch",
    "progress",
    "summary",
    "pr-link",
];

pub(crate) fn default_root() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| super::home().join(".claude"))
}

fn projects_root(root: &Path) -> PathBuf {
    let nested = root.join("projects");
    if nested.is_dir() {
        nested
    } else {
        root.to_owned()
    }
}

fn tool_result_body(block: &Value) -> String {
    match block.get("content") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => {
            let mut texts = Vec::new();
            let mut non_text = 0usize;
            for item in items {
                if text(item, "type") == "text" {
                    let body = text(item, "text");
                    if !body.trim().is_empty() {
                        texts.push(body.to_owned());
                    }
                } else {
                    non_text += 1;
                }
            }
            if non_text > 0 {
                texts.push(format!("[非文本工具结果未采集：{non_text} 项]"));
            }
            texts.join("\n")
        }
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn sidecar_custom_title(jsonl_path: &Path) -> Option<String> {
    let stem = jsonl_path.file_stem()?.to_str()?;
    let title_path = jsonl_path.parent()?.join(stem).join("custom-title.json");
    let raw = std::fs::read_to_string(title_path).ok()?;
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    let title = text(&parsed, "customTitle").trim();
    (!title.is_empty()).then(|| crate::common::redact(title))
}

fn is_subagent_file(path: &Path) -> bool {
    path.parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        == Some("subagents")
}

/// Read today's visible Claude Code CLI events and sessions under `root`.
pub(crate) fn collect(
    root: &Path,
    target_day: NaiveDate,
    timezone: Tz,
    cutoff: DateTime<Utc>,
) -> Result<super::SourceCapture> {
    let mut global_warnings = Vec::new();
    let scan_root = projects_root(root);
    if !scan_root.is_dir() {
        global_warnings.push(format!(
            "{LABEL_CLAUDE}：记录目录不存在或不可读（{}）",
            root.display()
        ));
        return Ok(super::SourceCapture {
            sessions: Vec::new(),
            events: Vec::new(),
            global_warnings,
        });
    }

    let day_start = local_day_start(target_day, timezone);
    let mut files = Vec::new();
    for entry in WalkDir::new(&scan_root).max_depth(4).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                global_warnings.push(format!(
                    "{LABEL_CLAUDE}：无法扫描部分记录目录，采集不完整"
                ));
                continue;
            }
        };
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(|ext| ext.to_str()) != Some("jsonl")
        {
            continue;
        }
        let path = entry.path().to_owned();
        if let Some(start) = day_start {
            if let Ok(modified) = std::fs::metadata(&path).and_then(|meta| meta.modified()) {
                let modified: DateTime<Utc> = modified.into();
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
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let subagent = is_subagent_file(&path);
        let fallback_parent = if subagent {
            path.parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned()
        } else {
            String::new()
        };

        let stream = match File::open(&path) {
            Ok(stream) => stream,
            Err(_) => {
                let sid = format!("{TOOL}:{stem}");
                session_info.insert(
                    sid.clone(),
                    json!({"id":sid,"title":"","project":"",
                        "warnings":[format!("{LABEL_CLAUDE}：无法读取 {rel}，采集不完整")],
                        "tool":TOOL,"toolLabel":LABEL_CLAUDE}),
                );
                continue;
            }
        };

        let mut raw_session_id = if subagent {
            fallback_parent.clone()
        } else {
            stem.clone()
        };
        let mut agent_id = if subagent {
            stem.strip_prefix("agent-").unwrap_or(&stem).to_owned()
        } else {
            String::new()
        };
        let mut project = String::new();
        let mut custom_title = if !subagent {
            sidecar_custom_title(&path)
        } else {
            None
        };
        let mut ai_title: Option<String> = None;
        let mut agent_name: Option<String> = None;
        let mut malformed = false;
        let mut local_warnings = Vec::<String>::new();
        let mut raw_records: Vec<RawRecord> = Vec::new();

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
                Ok(Value::Object(map)) => Value::Object(map),
                _ => {
                    malformed = true;
                    continue;
                }
            };
            let row_type = text(&row, "type");
            if !text(&row, "sessionId").is_empty() {
                raw_session_id = text(&row, "sessionId").to_owned();
            }
            if subagent && !text(&row, "agentId").is_empty() {
                agent_id = text(&row, "agentId").to_owned();
            }
            if project.is_empty() && !text(&row, "cwd").is_empty() {
                project = crate::common::redact(text(&row, "cwd"));
            }

            match row_type {
                "custom-title" => {
                    let value = text(&row, "customTitle").trim();
                    if !value.is_empty() {
                        custom_title = Some(crate::common::redact(value));
                    }
                    continue;
                }
                "ai-title" => {
                    let value = text(&row, "aiTitle").trim();
                    if !value.is_empty() {
                        ai_title = Some(crate::common::redact(value));
                    }
                    continue;
                }
                "agent-name" => {
                    let value = text(&row, "agentName").trim();
                    if !value.is_empty() {
                        agent_name = Some(crate::common::redact(value));
                    }
                    continue;
                }
                other if IGNORED_TYPES.contains(&other) => continue,
                "user" | "assistant" => {}
                _ => continue,
            }

            let Ok(parsed_ts) = DateTime::parse_from_rfc3339(text(&row, "timestamp")) else {
                malformed = true;
                continue;
            };
            let utc_ts = parsed_ts.with_timezone(&Utc);
            if parsed_ts.with_timezone(&timezone).date_naive() != target_day || utc_ts > cutoff {
                continue;
            }

            let Some(message) = row.get("message").filter(|v| v.is_object()) else {
                malformed = true;
                continue;
            };

            if row_type == "user" {
                if row.get("isMeta").and_then(Value::as_bool) == Some(true)
                    || text(&row, "promptSource") == "system"
                    || row.pointer("/origin/kind").and_then(Value::as_str)
                        == Some("task-notification")
                {
                    continue;
                }
                match message.get("content") {
                    Some(Value::String(s)) if !s.trim().is_empty() => {
                        raw_records.push(RawRecord {
                            line_number,
                            block_idx: 0,
                            utc_ts,
                            kind: "user".to_owned(),
                            body: s.clone(),
                            extra: None,
                        });
                    }
                    Some(Value::Array(blocks)) => {
                        for (block_idx, block) in blocks.iter().enumerate() {
                            match text(block, "type") {
                                "text" => {
                                    let body = text(block, "text");
                                    if !body.trim().is_empty() {
                                        raw_records.push(RawRecord {
                                            line_number,
                                            block_idx,
                                            utc_ts,
                                            kind: "user".to_owned(),
                                            body: body.to_owned(),
                                            extra: None,
                                        });
                                    }
                                }
                                "tool_result" => {
                                    let body = tool_result_body(block);
                                    if !body.trim().is_empty() {
                                        let mut extra = serde_json::Map::new();
                                        if block.get("is_error").and_then(Value::as_bool) == Some(true)
                                        {
                                            extra.insert("error".to_owned(), json!(true));
                                        }
                                        raw_records.push(RawRecord {
                                            line_number,
                                            block_idx,
                                            utc_ts,
                                            kind: "tool_result".to_owned(),
                                            body,
                                            extra: (!extra.is_empty()).then_some(extra),
                                        });
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            } else if row_type == "assistant" {
                match message.get("content") {
                    Some(Value::String(s)) if !s.trim().is_empty() => {
                        raw_records.push(RawRecord {
                            line_number,
                            block_idx: 0,
                            utc_ts,
                            kind: "assistant".to_owned(),
                            body: s.clone(),
                            extra: None,
                        });
                    }
                    Some(Value::Array(blocks)) => {
                        for (block_idx, block) in blocks.iter().enumerate() {
                            match text(block, "type") {
                                "thinking" | "redacted_thinking" => continue,
                                "text" => {
                                    let body = text(block, "text");
                                    if !body.trim().is_empty() {
                                        raw_records.push(RawRecord {
                                            line_number,
                                            block_idx,
                                            utc_ts,
                                            kind: "assistant".to_owned(),
                                            body: body.to_owned(),
                                            extra: None,
                                        });
                                    }
                                }
                                "tool_use" => {
                                    let name = text(block, "name").trim();
                                    if name.is_empty() {
                                        continue;
                                    }
                                    let mut body = name.to_owned();
                                    if let Some(input) = block.get("input").filter(|v| !v.is_null())
                                    {
                                        if let Ok(serialized) = serde_json::to_string(input) {
                                            if !serialized.is_empty() && serialized != "{}" {
                                                body.push('\n');
                                                body.push_str(&serialized);
                                            }
                                        }
                                    }
                                    raw_records.push(RawRecord {
                                        line_number,
                                        block_idx,
                                        utc_ts,
                                        kind: "tool_call".to_owned(),
                                        body,
                                        extra: None,
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        let session_id = if subagent {
            if raw_session_id.is_empty() {
                format!("{TOOL}:subagent:{agent_id}")
            } else {
                format!("{TOOL}:{raw_session_id}:subagent:{agent_id}")
            }
        } else {
            format!("{TOOL}:{raw_session_id}")
        };

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

        if malformed {
            local_warnings.push(format!(
                "{rel} 含损坏、未写完或无时间戳的记录；无法保证完整，稍后可重交"
            ));
        }

        let info = session_info.entry(session_id.clone()).or_insert_with(|| {
            let mut obj = json!({
                "id": session_id,
                "title": "",
                "project": "",
                "warnings": [],
                "tool": TOOL,
                "toolLabel": LABEL_CLAUDE,
            });
            if subagent && !raw_session_id.is_empty() {
                obj["parentSession"] = json!(format!("{TOOL}:{raw_session_id}"));
                obj["delegationDepth"] = json!(1);
            }
            obj
        });

        let resolved_title = custom_title
            .or(ai_title)
            .or_else(|| agent_name.map(|name| format!("子代理：{name}")));
        if let Some(title) = resolved_title {
            info["title"] = json!(title);
        }
        if !project.is_empty() && text(info, "project").is_empty() {
            info["project"] = json!(project);
        }
        let attached = info["warnings"].as_array_mut().unwrap();
        for warning in local_warnings {
            let value = json!(crate::common::redact(&warning));
            if !attached.contains(&value) {
                attached.push(value);
            }
        }
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
                sid.strip_prefix("claude:")
                    .unwrap_or(&sid)
                    .chars()
                    .take(12)
                    .collect::<String>()
            )
        } else {
            title.chars().take(100).collect::<String>()
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
                "daily-report-claude-{tag}-{}",
                crate::common::id()
            ));
            std::fs::create_dir_all(root.join("projects")).unwrap();
            Self { root }
        }

        fn write(&self, rel: &str, content: &str) {
            let path = self.root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, content).unwrap();
        }

        fn collect(&self, day: &str) -> crate::collector::SelectionCapture {
            collect_for_selection(
                &SourceRoots {
                    claude: Some(self.root.clone()),
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
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn claude_collects_visible_messages_tools_and_skips_thinking_and_meta() {
        let fixture = Fixture::new("visible");
        let lines = [
            json!({"type":"user","isSidechain":false,"cwd":"F:\\WORKSPACE\\demo","sessionId":"sess-1","timestamp":"2025-11-08T01:00:00Z","message":{"role":"user","content":"排查接口超时问题 sk-abcdefghijklmnopqrst"}}),
            json!({"type":"user","isMeta":true,"sessionId":"sess-1","timestamp":"2025-11-08T01:00:01Z","message":{"role":"user","content":"INTERNAL_META_MUST_NOT_APPEAR"}}),
            json!({"type":"user","promptSource":"system","sessionId":"sess-1","timestamp":"2025-11-08T01:00:02Z","message":{"role":"user","content":"INTERNAL_SYSTEM_PROMPT_MUST_NOT_APPEAR"}}),
            json!({"type":"assistant","sessionId":"sess-1","timestamp":"2025-11-08T01:01:00Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"PRIVATE_THINKING_MUST_NOT_APPEAR"},{"type":"tool_use","id":"tu-1","name":"Grep","input":{"pattern":"timeout"}}]}}),
            json!({"type":"user","sessionId":"sess-1","timestamp":"2025-11-08T01:01:05Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu-1","content":[{"type":"text","text":"命中 2 处超时"}],"is_error":true}]}}),
            json!({"type":"assistant","sessionId":"sess-1","timestamp":"2025-11-08T01:02:00Z","message":{"role":"assistant","content":[{"type":"text","text":"已定位下游超时原因"}]}}),
            json!({"type":"system","subtype":"turn_duration","durationMs":1200,"sessionId":"sess-1","timestamp":"2025-11-08T01:02:01Z"}),
            json!({"type":"ai-title","sessionId":"sess-1","aiTitle":"排查接口超时"}),
            json!({"type":"user","sessionId":"sess-1","timestamp":"2025-11-07T15:59:00Z","message":{"role":"user","content":"YESTERDAY_MUST_NOT_APPEAR"}}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        fixture.write("projects/F--WORKSPACE-demo/sess-1.jsonl", &lines);

        let result = fixture.collect("2025-11-08");
        let events = result.capture["events"].as_array().unwrap();
        assert_eq!(events.len(), 4, "events: {events:?}");
        assert_eq!(events[0]["kind"], "user");
        assert_eq!(events[1]["kind"], "tool_call");
        assert_eq!(events[2]["kind"], "tool_result");
        assert_eq!(events[2]["error"], true);
        assert_eq!(events[3]["kind"], "assistant");
        assert!(events.iter().all(|e| e["tool"] == "claude"));
        assert!(events.iter().all(|e| e["sessionId"] == "claude:sess-1"));

        let serialized = result.capture.to_string();
        for excluded in [
            "INTERNAL_META_MUST_NOT_APPEAR",
            "INTERNAL_SYSTEM_PROMPT_MUST_NOT_APPEAR",
            "PRIVATE_THINKING_MUST_NOT_APPEAR",
            "YESTERDAY_MUST_NOT_APPEAR",
            "sk-abcdefghijklmnopqrst",
        ] {
            assert!(!serialized.contains(excluded), "leaked: {excluded}");
        }
        assert!(serialized.contains("已定位下游超时原因"));
        assert_eq!(result.sessions.len(), 1);
        assert_eq!(result.sessions[0]["title"], "排查接口超时");
        assert_eq!(result.sessions[0]["project"], "F:\\WORKSPACE\\demo");
        assert_eq!(result.sessions[0]["toolLabel"], LABEL_CLAUDE);
    }

    #[test]
    fn claude_keeps_subagents_separate_and_uses_custom_title() {
        let fixture = Fixture::new("subagent");
        let main_lines = [
            json!({"type":"user","cwd":"F:\\WORKSPACE\\app","sessionId":"main-1","timestamp":"2025-11-08T02:00:00Z","message":{"role":"user","content":"主会话任务"}}),
            json!({"type":"ai-title","sessionId":"main-1","aiTitle":"AI 生成标题"}),
            json!({"type":"custom-title","sessionId":"main-1","customTitle":"自定义标题"}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        fixture.write("projects/app/main-1.jsonl", &main_lines);

        let sub_lines = [
            json!({"type":"user","cwd":"F:\\WORKSPACE\\app","sessionId":"main-1","agentId":"sub-9","isSidechain":true,"timestamp":"2025-11-08T02:01:00Z","message":{"role":"user","content":"检查子模块"}}),
            json!({"type":"assistant","sessionId":"main-1","agentId":"sub-9","isSidechain":true,"timestamp":"2025-11-08T02:02:00Z","message":{"role":"assistant","content":[{"type":"text","text":"子模块检查完成"}]}}),
            json!({"type":"agent-name","sessionId":"main-1","agentName":"代码检查代理"}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        fixture.write(
            "projects/app/main-1/subagents/agent-sub-9.jsonl",
            &sub_lines,
        );

        let result = fixture.collect("2025-11-08");
        assert_eq!(result.sessions.len(), 2, "sessions: {:?}", result.sessions);
        let main = result
            .sessions
            .iter()
            .find(|s| s["id"] == "claude:main-1")
            .unwrap();
        let sub = result
            .sessions
            .iter()
            .find(|s| s["id"] == "claude:main-1:subagent:sub-9")
            .unwrap();
        assert_eq!(main["title"], "自定义标题");
        assert_eq!(sub["title"], "子代理：代码检查代理");
        assert_eq!(sub["parentSession"], "claude:main-1");
        assert_eq!(sub["eventCount"], 2);
    }

    #[test]
    fn claude_truncates_oversized_event() {
        let fixture = Fixture::new("truncate");
        let big = "超长结果。".repeat(super::super::MAX_EVENT_CHARS);
        let lines = [json!({"type":"user","sessionId":"big-1","timestamp":"2025-11-08T03:00:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":big}]}})]
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fixture.write("projects/demo/big-1.jsonl", &lines);

        let result = fixture.collect("2025-11-08");
        let text = result.capture["events"][0]["text"].as_str().unwrap();
        assert!(text.contains("[已截断：原长"));
        assert!(text.contains("仅保留前 200000 字符]"));
    }
}
