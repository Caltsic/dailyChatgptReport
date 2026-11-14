// Local multi-source acceptance probe: reads only the given roots, prints counts and
// never uploads or stores session text. Use it to confirm a new collector against real
// local records; it is not part of the packaged applications.
use daily_report::collector::{collect_for_selection, SourceRoots};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    env,
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::Instant,
};
use walkdir::WalkDir;

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = env::args().collect();
    args.iter()
        .position(|value| value == name)
        .and_then(|index| args.get(index + 1).cloned())
}

fn reader(path: &Path) -> anyhow::Result<Box<dyn BufRead>> {
    let file = File::open(path)?;
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".zstd"))
    {
        Ok(Box::new(BufReader::new(
            zstd::stream::read::Decoder::new(file)?,
        )))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

/// Line references of DSH records that must never become visible work records. The
/// probe checks captured event references against these instead of guessing from text,
/// because the same words may legitimately appear in conversation content.
fn internal_references(dsh_root: &Path) -> anyhow::Result<HashSet<String>> {
    const INTERNAL: &[&str] = &[
        "system/message",
        "developer/message",
        "request/header",
        "request/context",
        "session/title-llm-request",
        "agent/inbox/spliced",
        "session-log-deepseek/delivery-accepted",
    ];
    let sessions = if dsh_root.join("sessions").is_dir() {
        dsh_root.join("sessions")
    } else {
        dsh_root.to_owned()
    };
    let mut references = HashSet::new();
    for entry in WalkDir::new(&sessions).min_depth(2).max_depth(3) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if !name.starts_with("session.v4") {
            continue;
        }
        let relative = path
            .strip_prefix(dsh_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        for (index, line) in reader(path)?.lines().enumerate() {
            let line = match line {
                Ok(line) => line,
                Err(_) => break,
            };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(row) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let kind = row.get("type").and_then(Value::as_str).unwrap_or("");
            if INTERNAL.contains(&kind) {
                references.insert(format!("{relative}:{}", index + 1));
            }
        }
    }
    Ok(references)
}

fn main() -> anyhow::Result<()> {
    let mut roots = SourceRoots::default();
    if let Some(path) = arg("--codex") {
        roots.codex = Some(PathBuf::from(path));
    }
    if let Some(path) = arg("--dsh") {
        roots.dsh = Some(PathBuf::from(path));
    }
    if roots.codex.is_none() && roots.dsh.is_none() {
        anyhow::bail!("请用 --codex <目录> 和/或 --dsh <目录> 指定要检查的本机记录目录");
    }
    let day = match arg("--day") {
        Some(day) => day,
        None => daily_report::common::today("Asia/Shanghai")?,
    };
    let zone = arg("--timezone").unwrap_or_else(|| "Asia/Shanghai".to_owned());
    let started = Instant::now();
    let capture = collect_for_selection(&roots, &day, &zone)?;
    let duration = started.elapsed().as_millis();
    let events = capture
        .capture["events"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut kinds = BTreeMap::<String, usize>::new();
    let mut tools = BTreeMap::<String, usize>::new();
    for event in &events {
        *kinds
            .entry(event["kind"].as_str().unwrap_or("?").to_owned())
            .or_insert(0) += 1;
        *tools
            .entry(event["tool"].as_str().unwrap_or("?").to_owned())
            .or_insert(0) += 1;
    }
    let sessions = capture
        .sessions
        .iter()
        .map(|session| {
            json!({"id":session["id"],"tool":session["tool"],"eventCount":session["eventCount"],
                "warnings":session["warnings"].as_array().map_or(0, Vec::len)})
        })
        .collect::<Vec<_>>();
    let internal_leaks = match &roots.dsh {
        Some(root) => {
            let internal = internal_references(root)?;
            events
                .iter()
                .filter(|event| {
                    internal.contains(event["source"].as_str().unwrap_or_default())
                })
                .count()
        }
        None => 0,
    };
    let result = json!({
        "day":day,
        "timeZone":zone,
        "captureSources":capture.capture["sources"],
        "eventKinds":kinds,
        "eventsByTool":tools,
        "events":events.len(),
        "sessions":sessions,
        "sessionCount":capture.sessions.len(),
        "warningCount":capture.capture["warnings"].as_array().map_or(0, Vec::len),
        "warnings":capture.capture["warnings"],
        "internalRecordLeaks":internal_leaks,
        "durationMs":duration,
        "uploaded":false
    });
    println!("{}", serde_json::to_string_pretty(&result)?);
    if internal_leaks > 0 {
        anyhow::bail!("DSH 内部记录进入了采集内容");
    }
    Ok(())
}
