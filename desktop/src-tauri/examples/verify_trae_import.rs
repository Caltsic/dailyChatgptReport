// Isolated acceptance for the manually exported Trae conversation import:
// collect a Trae Markdown export together with a Codex session, select, upload over
// actual HTTPS and validate the Leader snapshot.
mod support;
use anyhow::{ensure, Context, Result};
use daily_report::{common, leader::Leader, member::MemberClient};
use serde_json::{json, Value};
use std::{fs, io::Read, path::{Path, PathBuf}};

fn codex_fixture(root: &Path) -> Result<()> {
    fs::create_dir_all(root.join("sessions"))?;
    let lines = [
        json!({"type":"session_meta","payload":{"id":"codex-trae-demo","title":"Codex 会话","cwd":"C:\\Workspace\\Demo"}}),
        json!({"timestamp":common::now(),"type":"response_item","payload":{"type":"message","role":"user",
            "content":[{"type":"input_text","text":"CODEX_VISIBLE 处理导出功能"}]}}),
        json!({"timestamp":common::now(),"type":"response_item","payload":{"type":"message","role":"assistant","channel":"final",
            "content":[{"type":"output_text","text":"CODEX_ASSISTANT 已完成调整"}]}}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    fs::write(root.join("sessions/demo.jsonl"), format!("{lines}\n"))?;
    Ok(())
}

fn trae_fixture(root: &Path, day: &str) -> Result<()> {
    fs::create_dir_all(root)?;
    fs::write(
        root.join(format!("{day}-Trae 导出会话.md")),
        format!(
            "# Trae 导出会话\n导出时间：{day} 20:00:00\n## 用户\nTRAE_USER_VISIBLE 帮我排查接口超时\n## 助手\nTRAE_ASSISTANT_VISIBLE 已定位下游等待\n## 系统\nTRAE_SYSTEM_MUST_NOT_APPEAR\n"
        ),
    )?;
    Ok(())
}

fn capture(leader: &Leader, day: &str, version: u64) -> Result<Value> {
    let snapshot = leader.store.read(|c| Ok(common::all(c, "Snapshot", Some(day))?.into_iter()
        .find(|s| s["version"].as_u64() == Some(version)).context("Missing snapshot")?))?;
    let file = fs::File::open(leader.store.root.join("submissions").join(snapshot["fileName"].as_str().unwrap()))?;
    let mut text = String::new(); flate2::read::GzDecoder::new(file).read_to_string(&mut text)?;
    Ok(serde_json::from_str(&text)?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/acceptance/trae-import");
    let root = base.join(chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string());
    let codex_root = root.join("source-codex");
    let trae_root = root.join("source-trae");
    codex_fixture(&codex_root)?;
    let day = common::today("Asia/Shanghai")?;
    trae_fixture(&trae_root, &day)?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let leader = Leader::new(root.join("Leader"), root.clone())?;
    leader.dispatch("leader.configure", json!({"settings":{"id":common::id(),"name":"Trae 导入验证部门","timeZone":"Asia/Shanghai",
        "port":port,"addressMode":"manual","advertisedUrl":format!("https://127.0.0.1:{port}"),"apiBaseUrl":"","model":""},"apiKey":""})).await?;
    let employee = leader.dispatch("leader.addMember", json!({"name":"Trae 导入验证成员"})).await?;
    leader.clone().start().await?;
    let invitation = leader.dispatch("leader.invite", json!({"memberId":employee["id"]})).await?;
    let member = MemberClient::new(root.join("Member"))?;
    member.dispatch("member.pair", json!({"invitation":invitation["invitation"]})).await?;
    let configured = member.dispatch("member.sources", json!({"sources":[
        {"tool":"codex","enabled":true,"path":codex_root.to_string_lossy()},
        {"tool":"dsh","enabled":false},
        {"tool":"trae","enabled":true,"path":trae_root.to_string_lossy()}
    ]})).await?;
    ensure!(configured["sources"].as_array().unwrap().iter()
        .any(|source| source["tool"] == "trae" && source["enabled"] == true && source["detected"] == true),
        "Trae export source not enabled: {configured}");

    let draft = member.dispatch("member.collect", json!({"day":day})).await?;
    let sessions = draft["sessions"].as_array().context("Missing sessions")?;
    ensure!(sessions.len() == 2, "Expected Codex + Trae sessions: {sessions:?}");
    let trae_session = sessions.iter().find(|session| session["tool"] == "trae").context("Missing Trae session")?;
    ensure!(trae_session["title"] == "Trae 导出会话", "Trae title wrong: {trae_session}");
    ensure!(trae_session["eventCount"] == 2, "Trae event count wrong: {trae_session}");
    let preview = member.dispatch("member.previewSession",
        json!({"draftId":draft["draftId"],"sessionId":trae_session["id"]})).await?;
    let preview_text = preview.to_string();
    ensure!(preview_text.contains("TRAE_USER_VISIBLE") && preview_text.contains("TRAE_ASSISTANT_VISIBLE"),
        "Trae visible content missing in preview");
    ensure!(!preview_text.contains("TRAE_SYSTEM_MUST_NOT_APPEAR"), "Trae system section leaked into preview");
    ensure!(!preview_text.contains("导出时间"), "Export metadata leaked into preview");

    let selected = sessions.iter().map(|session| session["id"].clone()).collect::<Vec<_>>();
    let submitted = member.dispatch("member.submit",
        json!({"draftId":draft["draftId"],"sessionIds":selected})).await?;
    ensure!(submitted["received"] == true, "Leader did not receive Trae import: {submitted}");
    ensure!(submitted["sessionCount"] == 2 && submitted["eventCount"] == 4, "Submitted counts wrong: {submitted}");

    let uploaded = capture(&leader, &day, 1)?;
    ensure!(uploaded["protocol"] == 1, "Protocol changed");
    ensure!(uploaded["sessionCount"] == 2, "Snapshot session count wrong");
    let events = uploaded["events"].as_array().context("Missing events")?;
    ensure!(events.len() == 4, "Snapshot event count wrong: {}", events.len());
    ensure!(events.iter().any(|event| event["tool"] == "trae" && event["kind"] == "user"),
        "Snapshot missing Trae user event");
    ensure!(events.iter().any(|event| event["tool"] == "codex"), "Snapshot missing Codex event");
    let sources = uploaded["sources"].as_array().context("Snapshot missing source summary")?;
    ensure!(sources.iter().any(|source| source["tool"] == "trae")
        && sources.iter().any(|source| source["tool"] == "codex"), "Snapshot source summary wrong");
    let serialized = uploaded.to_string();
    ensure!(!serialized.contains("TRAE_SYSTEM_MUST_NOT_APPEAR"), "Snapshot contains Trae system section");
    ensure!(!serialized.contains("导出时间"), "Snapshot contains export metadata");

    leader.shutdown().await;
    let result = json!({"ok":true,"root":root,"day":day,"sources":configured,"draft":draft,
        "preview":preview,"submitted":submitted,"uploaded":uploaded,
        "verified":["Trae export folder enabled as a source","Markdown export parsed into user/assistant events",
            "system section and export metadata excluded","actual HTTPS upload accepted by Leader validation",
            "Codex and Trae sessions collected together in one snapshot"],"modelCalled":false});
    common::atomic_text(&base.join("latest-result.json"), &serde_json::to_string_pretty(&result)?)?;
    println!("Trae export import workflow passed: {}", base.display());
    Ok(())
}
