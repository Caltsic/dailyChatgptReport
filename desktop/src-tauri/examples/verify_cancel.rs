// One isolated cancellation workflow using the packaged Agent and a local streaming fixture.
use anyhow::{ensure, Context, Result};
use daily_report::{common::{self, all, get, put}, leader::Leader};
use serde_json::{json, Value};
use std::{io::Write, path::PathBuf, sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}}, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn wait_for(mut condition: impl FnMut() -> Result<bool>) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(60), async {
        while !condition()? { tokio::time::sleep(Duration::from_millis(50)).await; }
        Ok::<_, anyhow::Error>(())
    }).await.context("Cancellation fixture timed out")?
}

fn job(leader: &Leader, id: &str) -> Result<Value> {
    leader.store.read(|c| get(c, "Job", id)?.context("Missing job"))
}

async fn queue_personal(leader: &Leader, member: &str, day: &str) -> Result<Value> {
    let response = leader.dispatch("leader.generate", json!({"memberIds":[member],"day":day})).await?;
    ensure!(response["errors"].as_array().is_some_and(Vec::is_empty), "Unable to queue: {response}");
    leader.store.read(|c| all(c,"Job",Some(day))?.into_iter()
        .find(|j| j["memberId"] == member && matches!(j["state"].as_str(),Some("queued"|"running")))
        .context("No pending personal job"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/acceptance/tauri-cancel");
    let root = base.join(chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string());
    let resources = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/tauri-publish/Leader");
    let requests = Arc::new(AtomicUsize::new(0));
    let slow_started = Arc::new(AtomicUsize::new(0));
    let disconnected = Arc::new(AtomicUsize::new(0));
    let finish_fast = Arc::new(AtomicBool::new(false));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let model_port = listener.local_addr()?.port();
    let (calls, started, closed, fast) = (requests.clone(),slow_started.clone(),disconnected.clone(),finish_fast.clone());
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await?;
            let (calls,started,closed,fast) = (calls.clone(),started.clone(),closed.clone(),fast.clone());
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut byte = [0u8;1];
                while !request.ends_with(b"\r\n\r\n") {
                    ensure!(socket.read(&mut byte).await? == 1,"Incomplete mock request headers");
                    request.push(byte[0]);
                }
                let header = String::from_utf8(request)?;
                let length = header.lines().find_map(|line| {
                    let (name,value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length").then(||value.trim().parse::<usize>().ok()).flatten()
                }).context("Missing request length")?;
                let mut body = vec![0;length]; socket.read_exact(&mut body).await?;
                let request: Value = serde_json::from_slice(&body)?;
                let input: Value = serde_json::from_str(request["messages"].as_array().context("Missing messages")?.last().context("Empty messages")?["content"].as_str().context("No message text")?)?;
                calls.fetch_add(1,Ordering::SeqCst);
                let slow = !fast.load(Ordering::SeqCst) && input["employee"] != "并行完成成员";
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await?;
                let chunk = json!({"id":"cancel-fixture","object":"chat.completion.chunk","created":1,"model":"fixture","choices":[{"index":0,"delta":{"role":"assistant","content":if slow {"尚未完成的测试正文"}else{"# 软件验证日报\n\n本地样例生成已完成。"}},"finish_reason":null}]});
                socket.write_all(format!("data: {chunk}\n\n").as_bytes()).await?;
                if slow {
                    started.fetch_add(1,Ordering::SeqCst);
                    // No later chunks: only terminating the Agent can close this live request.
                    match socket.read(&mut byte).await {
                        Ok(0) => {},
                        Err(error) if matches!(error.kind(),std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted) => {},
                        other => anyhow::bail!("Expected cancelled connection to close: {other:?}"),
                    }
                    closed.fetch_add(1,Ordering::SeqCst);
                } else {
                    let end = json!({"id":"cancel-fixture","object":"chat.completion.chunk","created":1,"model":"fixture","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
                    socket.write_all(format!("data: {end}\n\ndata: [DONE]\n\n").as_bytes()).await?;
                }
                Ok::<_,anyhow::Error>(())
            });
        }
        #[allow(unreachable_code)] Ok::<_,anyhow::Error>(())
    });
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let leader = Leader::new(root.join("Leader"),resources.clone())?;
    leader.dispatch("leader.configure",json!({"settings":{"id":common::id(),"name":"取消验证部门","timeZone":"Asia/Shanghai","port":port,"addressMode":"manual","advertisedUrl":format!("https://127.0.0.1:{port}"),"apiBaseUrl":format!("http://127.0.0.1:{model_port}/v1"),"model":"fixture"},"apiKey":"fixture-key"})).await?;
    let day = common::today("Asia/Shanghai")?;
    let mut members = Vec::new();
    let mut old_reports = Vec::new();
    for name in ["取消验证成员","并行完成成员"] {
        let member = leader.dispatch("leader.addMember",json!({"name":name})).await?;
        let mid = member["id"].as_str().unwrap().to_owned();
        let sid = common::id();
        let capture = json!({"day":day,"timeZone":"Asia/Shanghai","capturedAt":common::now(),"sessionCount":1,"warnings":[],"events":[{"id":common::id(),"sessionId":"synthetic","timestamp":common::now(),"kind":"user","text":"隔离的软件取消流程验证。","source":"sessions/sample.jsonl:1"}]});
        let text = serde_json::to_string(&capture)?;
        let filename = format!("{sid}.json.gz");
        let mut gzip = flate2::write::GzEncoder::new(std::fs::File::create(leader.store.root.join("submissions").join(&filename))?,flate2::Compression::default());
        gzip.write_all(text.as_bytes())?; gzip.finish()?;
        let rid = common::id();
        let report = json!({"id":rid,"memberId":mid,"memberName":name,"department":"取消验证部门","day":day,"version":1,"snapshotId":sid,"snapshotVersion":1,"body":"已有日报原文保持不变。","createdAt":common::now(),"releasedAt":common::now(),"finalizedAt":null,"feedback":{"text":"无补充说明","noSupplement":true,"author":name,"submittedAt":common::now()},"sources":{},"content":{}});
        leader.store.write(|c| {
            put(c,"Snapshot",&sid,&json!({"id":sid,"memberId":mid,"day":day,"version":1,"hash":common::hash(&text),"fileName":filename,"receivedAt":common::now(),"capturedAt":capture["capturedAt"],"sessionCount":1,"warnings":[]}),&day)?;
            put(c,"Report",&rid,&report,&day)
        })?;
        members.push(mid); old_reports.push(report);
    }
    let old_summary_id = common::id();
    let old_summary = json!({"id":old_summary_id,"day":day,"version":1,"department":"取消验证部门","body":"已有汇总原文保持不变。","sources":{},"createdAt":common::now(),"sourceSignature":"fixture-prior"});
    leader.store.write(|c| put(c,"DepartmentReport",&old_summary_id,&old_summary,&day))?;

    let queued_personal = queue_personal(&leader,&members[0],&day).await?;
    let queued_summary = leader.dispatch("leader.summary",json!({"day":day})).await?;
    for queued in [&queued_personal,&queued_summary] {
        let result = leader.dispatch("leader.cancel",json!({"jobId":queued["id"]})).await?;
        ensure!(result["state"] == "cancelled" && result["finishedAt"].is_string(),"Queued cancellation failed");
    }
    ensure!(requests.load(Ordering::SeqCst)==0,"Queued cancellation invoked a model");
    let personal = queue_personal(&leader,&members[0],&day).await?;
    let summary = leader.dispatch("leader.summary",json!({"day":day})).await?;
    let parallel = queue_personal(&leader,&members[1],&day).await?;
    leader.clone().start().await?;
    wait_for(||Ok(slow_started.load(Ordering::SeqCst)==2)).await?;
    let running_dashboard = leader.dispatch("leader.dashboard",json!({"day":day})).await?;
    let start = std::time::Instant::now();
    for (index,active) in [&personal,&summary].into_iter().enumerate() {
        let result = leader.dispatch("leader.cancel",json!({"jobId":active["id"]})).await?;
        ensure!(result["state"]=="cancelled","Running cancellation failed");
        wait_for(||Ok(disconnected.load(Ordering::SeqCst)>=index+1)).await?;
        let repeated = leader.dispatch("leader.cancel",json!({"jobId":active["id"]})).await?;
        ensure!(repeated==result,"Repeated cancellation changed the terminal job");
    }
    let cancel_ms = start.elapsed().as_millis();
    wait_for(||Ok(job(&leader,parallel["id"].as_str().unwrap())?["state"]=="completed")).await?;
    leader.store.read(|c| {
        ensure!(get(c,"Report",old_reports[0]["id"].as_str().unwrap())?==Some(old_reports[0].clone()),"Cancelled generation altered prior report");
        ensure!(all(c,"Report",Some(&day))?.iter().filter(|r|r["memberId"]==members[0]).count()==1,"Cancelled partial report was published");
        ensure!(all(c,"DepartmentReport",Some(&day))?==vec![old_summary.clone()],"Cancelled summary was published");
        Ok(())
    })?;
    let cancelled_dashboard = leader.dispatch("leader.dashboard",json!({"day":day})).await?;
    finish_fast.store(true,Ordering::SeqCst);
    let fresh = queue_personal(&leader,&members[0],&day).await?;
    ensure!(fresh["id"]!=personal["id"],"Regeneration reused cancelled Agent workspace");
    wait_for(||Ok(job(&leader,fresh["id"].as_str().unwrap())?["state"]=="completed")).await?;
    let completed = job(&leader,fresh["id"].as_str().unwrap())?;
    let after_finish = leader.dispatch("leader.cancel",json!({"jobId":fresh["id"]})).await?;
    ensure!(after_finish==completed,"Cancelling a completed task changed its result");
    leader.shutdown().await; drop(leader);
    let restarted = Leader::new(root.join("Leader"),resources)?;
    restarted.clone().start().await?;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    for prior in [&queued_personal,&queued_summary,&personal,&summary] {
        ensure!(job(&restarted,prior["id"].as_str().unwrap())?["state"]=="cancelled","Restart resumed a cancelled task");
    }
    ensure!(requests.load(Ordering::SeqCst)==4,"Unexpected model call after cancel/restart");
    let dashboard = restarted.dispatch("leader.dashboard",json!({"day":day})).await?;
    finish_fast.store(false,Ordering::SeqCst);
    let paused = queue_personal(&restarted,&members[0],&day).await?;
    wait_for(||Ok(slow_started.load(Ordering::SeqCst)==3)).await?;
    restarted.shutdown().await;
    wait_for(||Ok(disconnected.load(Ordering::SeqCst)==3)).await?;
    ensure!(job(&restarted,paused["id"].as_str().unwrap())?["state"]=="queued","Shutdown did not requeue interrupted job");
    ensure!(requests.load(Ordering::SeqCst)==5,"Unexpected fixture request count");
    server.abort();
    let result = json!({"ok":true,"root":root,"day":day,"runningDashboard":running_dashboard,"cancelledDashboard":cancelled_dashboard,"dashboard":dashboard,"cancelMilliseconds":cancel_ms,"requests":requests.load(Ordering::SeqCst),"cancelledConnectionsClosed":2,"shutdownConnectionsClosed":1,"verified":["queued personal and department cancellation without request","running personal and department Agent streams closed","other parallel job completes","no partial reports published; old reports unchanged","repeat cancellation idempotent","fresh generation after cancellation","completed task wins cancellation","cancelled state survives restart","active shutdown closes process stream before requeue"],"model":"packaged Agent, five local fixture requests; no external models"});
    common::atomic_text(&base.join("latest-result.json"),&serde_json::to_string_pretty(&result)?)?;
    println!("Cancellation workflow passed: {}",base.display());
    Ok(())
}
