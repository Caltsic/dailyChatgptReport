mod support;
// Focused reproduction: old localhost settings -> detected LAN URL -> real member pairing.
use anyhow::{ensure, Context, Result};
use daily_report::{common, leader::Leader, member::MemberClient};
use serde_json::{json, Value};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/acceptance/tauri-network");
    let root = base.join(chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string());
    let port = std::net::TcpListener::bind("0.0.0.0:0")?.local_addr()?.port();
    let leader = Leader::new(root.join("Leader"), root.clone())?;
    let info = leader.dispatch("leader.init", json!({})).await?;
    let addresses = info["networkAddresses"].as_array().context("Missing network addresses")?;
    let ip = addresses.first().and_then(|a| a["address"].as_str()).context("No LAN address")?;
    let expected = format!("https://{ip}:{port}");
    // Initial configuration must work without entering an address.
    leader.dispatch("leader.configure", json!({"settings":{
        "id":common::id(),"name":"自动 IP 验证","timeZone":"Asia/Shanghai","port":port,
        "addressMode":"auto","advertisedUrl":"","apiBaseUrl":"","model":""
    },"apiKey":""})).await?;
    let configured = leader.dispatch("leader.init", json!({})).await?;
    ensure!(configured["settings"]["advertisedUrl"] == expected, "Fresh auto URL incorrect");
    let employee = leader.dispatch("leader.addMember", json!({"name":"网络验证成员"})).await?;
    let member_id = &employee["id"];
    leader.shutdown().await;

    // Reproduce the stored configuration from 0.3.0 rather than passing through new configure logic.
    leader.store.write(|c| {
        let mut settings = common::get(c, "DepartmentSettings", "settings")?.unwrap();
        settings["advertisedUrl"] = json!(format!("https://localhost:{port}"));
        settings.as_object_mut().unwrap().remove("addressMode");
        common::put(c, "DepartmentSettings", "settings", &settings, "")
    })?;
    drop(leader);
    let leader = Leader::new(root.join("Leader"), root.clone())?;
    leader.clone().start().await?;
    let init = leader.dispatch("leader.init", json!({})).await?;
    ensure!(init["settings"]["advertisedUrl"] == expected && init["settings"]["addressMode"] == "auto",
        "Legacy localhost was not migrated");
    let invitation = leader.dispatch("leader.invite", json!({"memberId":member_id})).await?;
    ensure!(invitation["details"]["url"] == expected, "Invitation did not use LAN address");
    let member = MemberClient::new(root.join("Member"))?;
    member.dispatch("member.pair", json!({"invitation":invitation["invitation"]})).await?;
    let status = member.dispatch("member.status", json!({"day":common::today("Asia/Shanghai")?})).await?;
    ensure!(status["online"] == true, "LAN pairing/status failed: {status}");
    let logs = root.join("sample-codex/sessions");
    std::fs::create_dir_all(&logs)?;
    std::fs::write(logs.join("sample.jsonl"), format!("{}\n{}\n",
        json!({"type":"session_meta","payload":{"id":"network-check"}}),
        json!({"timestamp":common::now(),"type":"response_item","payload":{"type":"message",
            "role":"user","content":[{"type":"input_text","text":"自动 IP 软件验证样例"}]}})))?;
    support::codex_only(&member, &root.join("sample-codex")).await?;
    let receipt = support::submit_selected(&member, &common::today("Asia/Shanghai")?).await?;
    ensure!(receipt["received"] == true && receipt["eventCount"] == 1, "LAN submission failed: {receipt}");

    // An address change must be picked up when creating an invitation, not only on startup.
    leader.store.write(|c| {
        let mut settings = common::get(c, "DepartmentSettings", "settings")?.unwrap();
        settings["advertisedUrl"] = json!(format!("https://192.0.2.1:{port}"));
        common::put(c, "DepartmentSettings", "settings", &settings, "")
    })?;
    let refreshed = leader.dispatch("leader.invite", json!({"memberId":member_id})).await?;
    ensure!(refreshed["details"]["url"] == expected, "Automatic invitation retained stale IP");

    let mut manual: Value = init["settings"].clone();
    manual["addressMode"] = json!("manual");
    let manual_url = format!("https://department.example:{port}");
    manual["advertisedUrl"] = json!(manual_url);
    leader.dispatch("leader.configure", json!({"settings":manual,"apiKey":""})).await?;
    let manual_init = leader.dispatch("leader.init", json!({})).await?;
    let manual_invite = leader.dispatch("leader.invite", json!({"memberId":member_id})).await?;
    ensure!(manual_init["settings"]["advertisedUrl"] == manual_url && manual_invite["details"]["url"] == manual_url,
        "Manual hostname was overwritten");
    leader.shutdown().await;
    let result = json!({"ok":true,"root":root,"detected":addresses,"pairedUrl":expected,
        "verified":["fresh automatic configuration","legacy localhost migration","actual LAN IP TLS pairing/status/upload",
            "refresh automatic URL on invitation","preserve manual hostname"],"modelCalled":false});
    common::atomic_text(&base.join("latest-result.json"), &serde_json::to_string_pretty(&result)?)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
