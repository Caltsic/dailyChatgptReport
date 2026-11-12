// Real Windows Leader side of the cross-platform acceptance: starts the HTTPS service,
// writes an invitation for the remote Linux member, waits for the upload, validates the
// capture with the production upload checks and reports counts only.
use anyhow::{bail, ensure, Context, Result};
use daily_report::{common, leader::Leader};
use serde_json::{json, Value};
use std::{fs, io::Read, path::PathBuf, time::{Duration, Instant}};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|value| value == name)
            .and_then(|index| args.get(index + 1).cloned())
    };
    let root = PathBuf::from(arg("--root").context("--root 必填")?);
    let port: u16 = arg("--port").context("--port 必填")?.parse()?;
    let invitation_out = PathBuf::from(arg("--invitation-out").context("--invitation-out 必填")?);
    let result_path = PathBuf::from(arg("--result").context("--result 必填")?);
    let zone = arg("--timezone").unwrap_or_else(|| "Asia/Shanghai".to_owned());
    let wait_seconds: u64 = arg("--wait-seconds")
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(180);
    fs::create_dir_all(&root)?;

    let day = common::today(&zone)?;
    let leader = Leader::new(root.join("Leader"), root.clone())?;
    leader
        .dispatch(
            "leader.configure",
            json!({"settings":{"id":common::id(),"name":"跨端 Linux 验证部门","timeZone":zone,"port":port,
                "addressMode":"manual","advertisedUrl":format!("https://127.0.0.1:{port}"),
                "apiBaseUrl":"","model":""},"apiKey":""}),
        )
        .await?;
    let member = leader
        .dispatch("leader.addMember", json!({"name":"跨端 Linux 验证成员"}))
        .await?;
    leader.clone().start().await?;
    let invitation = leader
        .dispatch("leader.invite", json!({"memberId":member["id"]}))
        .await?;
    common::atomic_text(&invitation_out, invitation["invitation"].as_str().context("邀请为空")?)?;
    println!("leader ready: https://127.0.0.1:{port} day={day} invitation={}", invitation_out.display());

    let started = Instant::now();
    let snapshot = loop {
        if started.elapsed() > Duration::from_secs(wait_seconds) {
            leader.shutdown().await;
            bail!("等待远端成员上传超时（{wait_seconds} 秒）");
        }
        let found = leader.store.read(|c| {
            Ok(common::all(c, "Snapshot", Some(&day))?
                .into_iter()
                .max_by_key(|value| value["version"].as_u64().unwrap_or(0)))
        })?;
        if let Some(snapshot) = found {
            break snapshot;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    let file = leader
        .store
        .root
        .join("submissions")
        .join(snapshot["fileName"].as_str().context("快照文件名缺失")?);
    let mut text = String::new();
    flate2::read::GzDecoder::new(fs::File::open(file)?).read_to_string(&mut text)?;
    let capture: Value = serde_json::from_str(&text)?;
    let validation = leader.validate_capture_value(&capture);
    ensure!(validation.is_ok(), "采集内容未通过组长端校验：{:?}", validation.err());

    let events = capture["events"].as_array().cloned().unwrap_or_default();
    let mut kinds = std::collections::BTreeMap::<String, usize>::new();
    let mut tools = std::collections::BTreeMap::<String, usize>::new();
    for event in &events {
        *kinds
            .entry(event["kind"].as_str().unwrap_or("?").to_owned())
            .or_insert(0) += 1;
        *tools
            .entry(event["tool"].as_str().unwrap_or("?").to_owned())
            .or_insert(0) += 1;
    }
    let result = json!({
        "ok":true,
        "day":day,
        "timeZone":zone,
        "memberId":snapshot["memberId"],
        "snapshotVersion":snapshot["version"],
        "protocol":capture["protocol"],
        "sessionCount":capture["sessionCount"],
        "eventCount":events.len(),
        "eventKinds":kinds,
        "eventsByTool":tools,
        "captureSources":capture["sources"],
        "warnings":capture["warnings"].as_array().map_or(0, Vec::len),
        "validatedBy":"leader.validate_capture",
        "verified":[
            "real Windows Leader received the upload over HTTPS",
            "capture passed the same checks as the production upload route",
            "capture protocol/timezone/day/session count/event kinds are valid"
        ],
        "modelCalled":false
    });
    common::atomic_text(&result_path, &serde_json::to_string_pretty(&result)?)?;
    println!("leader validation passed: {}", result_path.display());
    leader.shutdown().await;
    Ok(())
}
