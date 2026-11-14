mod support;
// One actual MemberClient -> HTTPS -> SQLite check for the desktop migration.
use anyhow::{ensure, Context, Result};
use daily_report::{common, leader::Leader, member::MemberClient};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

#[tokio::main]
async fn main() -> Result<()> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../artifacts/acceptance/tauri-member")
        .canonicalize()
        .or_else(|_| {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../artifacts/acceptance/tauri-member");
            fs::create_dir_all(&path)?;
            path.canonicalize()
        })?;
    let root = base.join(chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string());
    let leader_root = root.join("Leader");
    let member_root = root.join("Member");
    let codex_root = root.join("sample-codex");
    fs::create_dir_all(codex_root.join("sessions"))?;
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.local_addr()?.port()
    };
    let day = common::today("Asia/Shanghai")?;
    let leader = Leader::new(leader_root.clone(), root.clone())?;
    leader
        .dispatch(
            "leader.configure",
            json!({"settings":{
        "id":common::id(),"name":"Tauri 界面验证","timeZone":"Asia/Shanghai",
        "port":port,"advertisedUrl":format!("https://127.0.0.1:{port}"),
        "apiBaseUrl":"http://127.0.0.1:1","model":"ui-check"
    },"apiKey":""}),
        )
        .await?;
    let employee = leader
        .dispatch("leader.addMember", json!({"name":"界面验证成员"}))
        .await?;
    let member_id = employee["id"].as_str().context("Missing member id")?;
    let invitation = leader
        .dispatch("leader.invite", json!({"memberId":member_id}))
        .await?;
    let member = MemberClient::new(member_root.clone())?;
    member
        .dispatch(
            "member.pair",
            json!({"invitation":invitation["invitation"]}),
        )
        .await?;
    let initial = member.dispatch("member.status", json!({"day":day})).await?;
    ensure!(
        initial["online"] == true,
        "MemberClient pairing/status did not connect"
    );
    ensure!(
        initial["status"]["receipt"].is_null(),
        "Unexpected preexisting receipt"
    );

    let sample = [
        json!({"type":"session_meta","payload":{"id":"ui-sample-session"}}),
        json!({"timestamp":common::now(),"type":"response_item","payload":{
            "type":"message","role":"user","content":[{"type":"input_text","text":"界面验证样例：检查日报软件，不代表实际员工工作。"}]
        }})
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
    fs::write(codex_root.join("sessions/sample.jsonl"), sample)?;
    member
        .dispatch("member.codexRoot", json!({"path":codex_root}))
        .await?;
    support::codex_only(&member, &codex_root).await?;
    let submission = support::submit_selected(&member, &day).await?;
    ensure!(
        submission["received"] == true && submission["eventCount"] == 1,
        "MemberClient did not submit sample: {submission}"
    );
    let received = member.dispatch("member.status", json!({"day":day})).await?;
    ensure!(
        received["status"]["receipt"]["version"] == 1,
        "First receipt is missing"
    );

    leader.shutdown().await;
    drop(leader);
    let offline = support::submit_selected(&member, &day).await?;
    ensure!(
        offline["received"] == false,
        "Offline submission must remain queued"
    );
    drop(member);
    let member = MemberClient::new(member_root.clone())?;
    let restored = member.dispatch("member.init", json!({})).await?;
    ensure!(
        restored["queue"].as_array().map(Vec::len) == Some(1),
        "Persisted outbox not restored"
    );
    let leader = Leader::new(leader_root.clone(), root.clone())?;
    leader.clone().start().await?;
    let recovered = member.dispatch("member.status", json!({"day":day})).await?;
    ensure!(
        recovered["online"] == true && recovered["status"]["receipt"]["version"] == 2,
        "Restarted MemberClient did not flush: {recovered}"
    );
    ensure!(
        recovered["queue"].as_array().is_some_and(Vec::is_empty),
        "Recovered outbox not empty"
    );
    member.dispatch("member.status", json!({"day":day})).await?;
    let snapshot = leader.store.read(|connection| {
        let snapshots = common::all(connection, "Snapshot", Some(&day))?;
        ensure!(
            snapshots.len() == 2,
            "Retry applied the same submission twice"
        );
        snapshots
            .into_iter()
            .max_by_key(|value| value["version"].as_i64().unwrap_or(0))
            .context("Snapshot is missing")
    })?;

    // Keep a released, unfinalized fixture for the subsequent real-window editability check.
    let report_id = common::id();
    let report = json!({
        "id":report_id,"memberId":member_id,"memberName":"界面验证成员","department":"Tauri 界面验证",
        "day":day,"version":1,"snapshotId":snapshot["id"],"snapshotVersion":snapshot["version"],
        "capturedAt":snapshot["capturedAt"],"sessionCount":snapshot["sessionCount"],"warnings":snapshot["warnings"],
        "body":"# 界面验证\n\n这是隔离界面样例，不代表员工实际工作。\n\n## 检查内容\n\n正文只读；个人补充说明可以编辑。",
        "content":{"tasks":[],"outputs":[],"verification":[],"blockers":[],"nextSteps":[]},
        "sources":{},"promptVersion":"ui-fixture","model":"ui-fixture","createdAt":common::now(),
        "releasedAt":common::now(),"finalizedAt":null,"feedback":null,"timeZone":"Asia/Shanghai"
    });
    leader
        .store
        .write(|connection| common::put(connection, "Report", &report_id, &report, &day))?;
    let visible = member.dispatch("member.status", json!({"day":day})).await?;
    ensure!(
        visible["status"]["report"]["id"] == report_id,
        "Released report was not cached on member"
    );
    leader.shutdown().await;
    let result = json!({"ok":true,"leaderRoot":leader_root,"memberRoot":member_root,"port":port,"day":day,
        "reportId":report_id,"verified":["MemberClient TLS pin + pairing","actual collector upload receipt",
        "offline durable queue","member and leader restart","recovery without duplicate submission"],
        "modelCalled":false});
    common::atomic_text(
        &root.join("result.json"),
        &serde_json::to_string_pretty(&result)?,
    )?;
    common::atomic_text(
        &base.join("latest-result.json"),
        &serde_json::to_string_pretty(&result)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
