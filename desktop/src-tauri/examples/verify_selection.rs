// A single HTTPS workflow for local selection, frozen upload, retry and empty-record semantics.
use anyhow::{ensure, Context, Result};
use daily_report::{common, leader::Leader, member::MemberClient};
use serde_json::{json, Value};
use std::{fs, io::{Read, Write}, path::{Path, PathBuf}};

fn message(session: &str, title: &str, project: &str, body: &str) -> String {
    format!("{}\n{}\n", json!({"type":"session_meta","payload":{"id":session,"title":title,"cwd":project}}),
        json!({"timestamp":common::now(),"type":"response_item","payload":{"type":"message","role":"user",
            "content":[{"type":"input_text","text":body}]}}))
}
fn ids(draft: &Value) -> Vec<String> {
    draft["sessions"].as_array().unwrap().iter().filter(|s|s["selected"] == true)
        .map(|s|s["id"].as_str().unwrap().to_owned()).collect()
}
fn capture(leader: &Leader, day: &str, version: u64) -> Result<Value> {
    let snapshot = leader.store.read(|c| Ok(common::all(c, "Snapshot", Some(day))?.into_iter()
        .find(|s| s["version"].as_u64() == Some(version)).context("Missing snapshot")?))?;
    let file = fs::File::open(leader.store.root.join("submissions").join(snapshot["fileName"].as_str().unwrap()))?;
    let mut text = String::new(); flate2::read::GzDecoder::new(file).read_to_string(&mut text)?;
    Ok(serde_json::from_str(&text)?)
}
fn queued_capture(root: &Path) -> Result<Value> {
    let paths = fs::read_dir(root.join("outbox"))?.filter_map(|e|e.ok()).filter(|e|e.path().extension().is_some_and(|x|x == "secret")).collect::<Vec<_>>();
    ensure!(paths.len() == 1, "Duplicate or missing queued upload");
    let item: Value = serde_json::from_str(&common::read_secret(&paths[0].path())?)?;
    let payload: Value = serde_json::from_str(item["payload"].as_str().unwrap())?;
    ensure!(payload["hash"] == common::hash(&payload["capture"].to_string()), "Filtered hash incorrect");
    Ok(payload["capture"].clone())
}

#[tokio::main]
async fn main() -> Result<()> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/acceptance/tauri-selection");
    let root = base.join(chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string());
    let leader_root = root.join("Leader"); let member_root = root.join("Member");
    let logs = root.join("sample-codex");
    fs::create_dir_all(logs.join("sessions"))?; fs::create_dir_all(logs.join("archived_sessions"))?;
    fs::write(logs.join("sessions/a.jsonl"), message("session-a","选中的项目讨论","C:\\Workspace\\Selected","整理今日任务 sk-abcdefghijklmnopqrst"))?;
    let tool_text = format!("{}完整工具结果末尾", "样例工具输出。".repeat(1500));
    let tool = json!({"timestamp":common::now(),"type":"response_item","payload":{"type":"function_call_output","output":tool_text}});
    writeln!(fs::OpenOptions::new().append(true).open(logs.join("sessions/a.jsonl"))?, "{tool}")?;
    fs::write(logs.join("sessions/private-excluded.jsonl"), message("session-b","PRIVATE_EXCLUDED_TITLE","C:\\PRIVATE_EXCLUDED_PROJECT","PRIVATE_EXCLUDED_BODY"))?;
    let unsupported = json!({"timestamp":common::now(),"type":"response_item","payload":{"type":"future_payload"}});
    writeln!(fs::OpenOptions::new().append(true).open(logs.join("sessions/private-excluded.jsonl"))?, "{unsupported}")?;
    fs::write(logs.join("archived_sessions/c.jsonl"), message("session-c","归档会话继续工作","C:\\Workspace\\Selected","当天继续处理归档会话"))?;
    let day = common::today("Asia/Shanghai")?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let leader = Leader::new(leader_root.clone(), root.clone())?;
    leader.dispatch("leader.configure", json!({"settings":{"id":common::id(),"name":"筛选验证部门","timeZone":"Asia/Shanghai",
        "port":port,"addressMode":"manual","advertisedUrl":format!("https://127.0.0.1:{port}"),"apiBaseUrl":"","model":""},"apiKey":""})).await?;
    let employee = leader.dispatch("leader.addMember", json!({"name":"筛选验证成员"})).await?;
    let invitation = leader.dispatch("leader.invite", json!({"memberId":employee["id"]})).await?;
    let member = MemberClient::new(member_root.clone())?;
    member.dispatch("member.pair", json!({"invitation":invitation["invitation"]})).await?;
    member.dispatch("member.codexRoot", json!({"path":logs})).await?;
    let initial = member.dispatch("member.collect", json!({"day":day})).await?;
    ensure!(ids(&initial).len() == 3, "First collection must select all sessions");
    let mut previews = serde_json::Map::new();
    for id in ["session-a", "session-b", "session-c"] {
        let preview = member.dispatch("member.previewSession", json!({"draftId":initial["draftId"],"sessionId":id})).await?;
        previews.insert(id.to_owned(), preview);
    }
    ensure!(!previews["session-a"].to_string().contains("sk-abcdefghijklmnopqrst")
        && previews["session-a"].to_string().contains("完整工具结果末尾"), "Preview redaction or complete content failed");
    let chosen = json!(["session-a", "session-c"]);
    member.dispatch("member.saveSelection", json!({"draftId":initial["draftId"],"sessionIds":chosen})).await?;
    let before = member.dispatch("member.init", json!({})).await?;
    ensure!(before["queue"].as_array().unwrap().is_empty(), "Collect/preview enqueued before confirmation");
    ensure!(leader.store.read(|c| Ok(common::all(c, "Snapshot", Some(&day))?.is_empty()))?, "Unexpected upload before confirmation");

    let added = json!({"timestamp":common::now(),"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"AFTER_PREVIEW_MUST_NOT_UPLOAD"}]}});
    writeln!(fs::OpenOptions::new().append(true).open(logs.join("sessions/a.jsonl"))?, "{added}")?;
    fs::write(logs.join("sessions/d.jsonl"), message("session-d","新增会话","C:\\Workspace\\New","NEW_SESSION_MUST_NOT_UPLOAD"))?;
    leader.shutdown().await; drop(leader);
    let request = json!({"draftId":initial["draftId"],"sessionIds":chosen});
    let submitted = member.dispatch("member.submit", request.clone()).await?;
    ensure!(submitted["received"] == false, "Offline upload not queued");
    member.dispatch("member.submit", request).await?;
    let queued = queued_capture(&member_root)?;
    ensure!(queued["sessionCount"] == 2 && queued["events"].as_array().unwrap().len() == 3, "Filtered counts incorrect");
    for marker in ["PRIVATE_EXCLUDED", "private-excluded", "AFTER_PREVIEW_MUST_NOT_UPLOAD", "NEW_SESSION_MUST_NOT_UPLOAD", "sk-abcdefghijklmnopqrst"] {
        ensure!(!queued.to_string().contains(marker), "Unexpected queued content: {marker}");
    }
    drop(member);
    let member = MemberClient::new(member_root.clone())?;
    let leader = Leader::new(leader_root.clone(), root.clone())?;
    leader.clone().start().await?;
    let status = member.dispatch("member.status", json!({"day":day})).await?;
    ensure!(status["status"]["receipt"]["version"] == 1 && status["queue"].as_array().unwrap().is_empty(), "Restart did not deliver exactly one version");
    ensure!(capture(&leader, &day, 1)? == queued, "Queued preview contents changed on retry");
    ensure!(!status["status"]["submissionStatus"].as_str().unwrap().contains("异常"), "Selection incorrectly reported as collection anomaly");
    let refreshed = member.dispatch("member.collect", json!({"day":day})).await?;
    let selected = ids(&refreshed);
    ensure!(selected.len() == 2 && selected.contains(&"session-a".to_owned()) && selected.contains(&"session-c".to_owned()), "Same-day selection not restored");
    let new_session = refreshed["sessions"].as_array().unwrap().iter().find(|s|s["id"] == "session-d").unwrap();
    ensure!(new_session["isNew"] == true && new_session["selected"] == false, "New session silently selected");
    member.dispatch("member.submit", json!({"draftId":refreshed["draftId"],"sessionIds":[]})).await?;
    let empty_selected = capture(&leader, &day, 2)?;
    ensure!(empty_selected["sessionCount"] == 0 && empty_selected["events"].as_array().unwrap().is_empty(), "Intentional empty upload contains data");
    ensure!(empty_selected.to_string().contains("本次未选择上传会话") && !empty_selected.to_string().contains("未发现可用工作记录")
        && !empty_selected.to_string().contains("PRIVATE_EXCLUDED"), "Intentional-empty explanation incorrect");
    let empty_logs = root.join("empty-codex"); fs::create_dir_all(empty_logs.join("sessions"))?;
    member.dispatch("member.codexRoot", json!({"path":empty_logs})).await?;
    let empty = member.dispatch("member.collect", json!({"day":day})).await?;
    member.dispatch("member.submit", json!({"draftId":empty["draftId"],"sessionIds":[]})).await?;
    let empty_found = capture(&leader, &day, 3)?;
    ensure!(empty_found.to_string().contains("未发现可用工作记录"), "No-records explanation missing");
    leader.shutdown().await;
    let result = json!({"ok":true,"root":root,"day":day,"initial":initial,"previews":previews,"refreshed":refreshed,
        "empty":empty,"memberInitialization":before,"offlineResult":submitted,
        "verified":["no upload before confirmation","complete redacted session preview","excluded content/metadata/warnings absent",
            "frozen snapshot","single offline upload on repeat click","restart sends identical selected data",
            "same-day choices remembered; new sessions unselected","selection is not collection anomaly","intentional-empty versus no-records"],"modelCalled":false});
    common::atomic_text(&base.join("latest-result.json"), &serde_json::to_string_pretty(&result)?)?;
    println!("Session selection workflow passed: {}", base.display());
    Ok(())
}
