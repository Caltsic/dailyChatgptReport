use anyhow::{bail, Context};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;

pub mod codex;
pub mod dsh;
pub mod trae;

/// Labels shown in the member UI and used as collection warning prefixes.
pub(crate) const LABEL_CODEX: &str = "Codex";
pub(crate) const LABEL_DSH: &str = "DeepSeek Harness";
pub(crate) const LABEL_TRAE: &str = "Trae 导出";

/// A single visible record above this many characters is cut before upload. The cut
/// stays visible in the text and the member scope note mentions the limit, so reports
/// can qualify what was shortened. The full local copy is never modified.
pub const MAX_EVENT_CHARS: usize = 200_000;

/// Enforced per-event limit note appended to the member scope warning.
pub const TRUNCATION_NOTE: &str = "超过 200000 字符的单条记录已截断并在原文标注";

/// Roots to read for one collection. `None` means the source is disabled or missing.
#[derive(Clone, Debug, Default)]
pub struct SourceRoots {
    pub codex: Option<PathBuf>,
    pub dsh: Option<PathBuf>,
    /// Folder containing conversations exported manually from the Trae IDE.
    pub trae: Option<PathBuf>,
}

pub struct SelectionCapture {
    pub capture: Value,
    pub sessions: Vec<Value>,
    pub global_warnings: Vec<String>,
}

pub(crate) struct SourceCapture {
    pub sessions: Vec<Value>,
    pub events: Vec<Value>,
    pub global_warnings: Vec<String>,
}

pub(crate) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

pub(crate) fn block_text(blocks: Option<&Value>) -> String {
    blocks
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| matches!(text(block, "type"), "input_text" | "output_text" | "text"))
                .map(|block| text(block, "text"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Cut one event body at the shared limit, leaving an explicit marker in the text.
pub(crate) fn truncate(body: String) -> String {
    let total = body.chars().count();
    if total <= MAX_EVENT_CHARS {
        return body;
    }
    let kept: String = body.chars().take(MAX_EVENT_CHARS).collect();
    format!("{kept}\n[已截断：原长 {total} 字符，仅保留前 {MAX_EVENT_CHARS} 字符]")
}

pub(crate) fn home() -> PathBuf {
    // Windows normally exposes USERPROFILE; Unix tools set HOME. Keep the other as fallback.
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    home.map(PathBuf::from).unwrap_or_default()
}

pub fn default_root() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".codex"))
}

pub fn default_dsh_root() -> PathBuf {
    std::env::var_os("DSH_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".dsh"))
}

pub fn local_day_start(day: NaiveDate, timezone: Tz) -> Option<DateTime<Utc>> {
    let naive = day.and_hms_opt(0, 0, 0)?;
    timezone
        .from_local_datetime(&naive)
        .earliest()
        .map(|value| value.with_timezone(&Utc))
}

pub fn collect(roots: &SourceRoots, day: &str, zone: &str) -> anyhow::Result<Value> {
    Ok(collect_for_selection(roots, day, zone)?.capture)
}

pub fn collect_for_selection(roots: &SourceRoots, day: &str, zone: &str) -> anyhow::Result<SelectionCapture> {
    let target_day =
        NaiveDate::parse_from_str(day, "%Y-%m-%d").context("日期格式应为 YYYY-MM-DD")?;
    let timezone: Tz = crate::common::timezone(zone).context("部门时区无效")?;
    if day > crate::common::today(zone)?.as_str() {
        bail!("不能提交未来日期");
    }
    let cutoff = Utc::now();
    let captured_at = cutoff.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    let mut captured = Vec::<(&str, &str, SourceCapture)>::new();
    if let Some(root) = &roots.codex {
        captured.push((
            "codex",
            LABEL_CODEX,
            codex::collect(root, target_day, timezone, cutoff)?,
        ));
    }
    if let Some(root) = &roots.dsh {
        captured.push((
            "dsh",
            LABEL_DSH,
            dsh::collect(root, target_day, timezone, cutoff)?,
        ));
    }
    if let Some(root) = &roots.trae {
        captured.push((
            "trae",
            LABEL_TRAE,
            trae::collect(root, target_day, timezone, cutoff)?,
        ));
    }
    let mut sessions = Vec::new();
    let mut events = Vec::new();
    let mut global_warnings = Vec::new();
    let mut sources = Vec::new();
    for (tool, label, capture) in captured {
        let session_count = capture
            .events
            .iter()
            .map(|event| text(event, "sessionId"))
            .collect::<HashSet<_>>()
            .len();
        sources.push(json!({"tool":tool,"label":label,"sessionCount":session_count,
            "eventCount":capture.events.len()}));
        sessions.extend(capture.sessions);
        events.extend(capture.events);
        global_warnings.extend(capture.global_warnings);
    }
    let mut unique = HashSet::new();
    events.retain(|event| unique.insert(text(event, "id").to_owned()));
    events.sort_by_cached_key(|event| {
        DateTime::parse_from_rfc3339(text(event, "timestamp"))
            .map(|value| value.timestamp_micros())
            .unwrap_or(i64::MAX)
    });
    let session_count = events
        .iter()
        .map(|event| text(event, "sessionId"))
        .collect::<HashSet<_>>()
        .len();
    sessions.sort_by(|a, b| {
        text(b, "lastAt")
            .cmp(text(a, "lastAt"))
            .then(text(a, "id").cmp(text(b, "id")))
    });
    global_warnings.push("仅覆盖本机可读取的当日事件；跨日上下文、附件及外部文件未采集。".to_owned());
    let mut warnings = global_warnings.clone();
    for session in &sessions {
        warnings.extend(
            session["warnings"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
    }
    if events.is_empty() {
        warnings.push("未发现可用工作记录；不代表当天没有工作".to_owned());
    }
    let mut seen = HashSet::new();
    warnings.retain(|warning| seen.insert(warning.clone()));
    Ok(SelectionCapture {
        capture: json!({"day":day,"timeZone":zone,"capturedAt":captured_at,"events":events,
            "warnings":warnings,"sessionCount":session_count,"protocol":1,"sources":sources}),
        sessions,
        global_warnings,
    })
}
