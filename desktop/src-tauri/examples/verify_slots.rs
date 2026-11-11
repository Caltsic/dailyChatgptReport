// One process-level fixture for shared model slots, cancellation, retry and evidence persistence.
use anyhow::{ensure, Context, Result};
use daily_report::{common::{self, all, get, put}, leader::Leader};
use serde_json::{json, Value};
use std::{collections::HashMap, io::Write, path::PathBuf, sync::{Arc, Mutex}, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

fn emit(value: Value) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}

async fn synthetic_agent() -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let request: Value = serde_json::from_str(&lines.next_line().await?.context("Missing host request")?)?;
    ensure!(request["slotProtocol"] == true, "Host omitted slot protocol");
    ensure!(request["prompts"]["extract"].is_string() && request["prompts"]["merge"].is_string(), "Missing analysis prompts");
    ensure!(request["api"]["contextTokens"] == 65_536 && request["api"]["outputReserveTokens"] == 8_192 && request["api"]["chunkTokens"] == 48_000, "Missing capacity defaults");
    let jid = request["jobId"].as_str().unwrap().to_owned();
    let workspace = PathBuf::from(request["workspace"].as_str().unwrap());
    std::fs::create_dir_all(&workspace)?;
    let retry = workspace.join("attempt.marker").exists();
    std::fs::write(workspace.join("attempt.marker"), b"ready")?;
    let port = url::Url::parse(request["api"]["baseUrl"].as_str().unwrap())?.port().context("Missing fixture port")?;
    emit(json!({"type":"progress","stage":"分段抽取","message":"6 个片段等待分析","completed":0,"total":6,"active":0,"failed":0,"estimatedTokens":123456,"requests":0,"retries":if retry {1}else{0},"pieces":[{"id":"piece-1","label":"会话第一段","state":"queued"}]}));
    for index in 0..6 {
        emit(json!({"type":"request_slot","requestId":format!("{jid}-{index}")}));
    }
    let mut pending = tokio::task::JoinSet::new();
    let mut completed = 0;
    while completed < 6 {
        tokio::select! {
            line = lines.next_line() => {
                let event: Value = serde_json::from_str(&line?.context("Host closed slot channel")?)?;
                ensure!(event["type"] == "slot", "Unexpected host event");
                let rid = event["requestId"].as_str().unwrap().to_owned();
                let jid = jid.clone();
                pending.spawn(async move {
                    let mut connection = tokio::net::TcpStream::connect(("127.0.0.1",port)).await?;
                    connection.write_all(format!("{jid}\n").as_bytes()).await?;
                    let mut done = [0u8;1];
                    connection.read_exact(&mut done).await?;
                    emit(json!({"type":"release_slot","requestId":rid}));
                    Ok::<_,anyhow::Error>(())
                });
            }
            done = pending.join_next(), if !pending.is_empty() => {
                done.context("Missing request")???;
                completed += 1;
                emit(json!({"type":"progress","stage":"分段抽取","message":format!("已完成 {completed}/6"),"completed":completed,"total":6,"active":pending.len(),"requests":completed,"pieces":[{"id":"piece-1","label":"会话第一段","state":"completed"}]}));
            }
        }
    }
    emit(json!({"type":"evidence","sources":{"event-1:0-100":"会话第一段原文"},"coverage":{"completed":6,"total":6,"complete":true}}));
    if retry {
        emit(json!({"type":"result","text":"# 软件验证日报\n\n六个片段分析完成，证据来自隔离的协议样例。"}));
    } else {
        emit(json!({"type":"error","message":"用于验证重试释放槽位的预期失败"}));
    }
    Ok(())
}

async fn wait_for(mut condition: impl FnMut() -> Result<bool>) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !condition()? { tokio::time::sleep(Duration::from_millis(25)).await; }
        Ok::<_,anyhow::Error>(())
    }).await.context("Slot workflow timed out")?
}

fn job(leader: &Leader, id: &str) -> Result<Value> {
    leader.store.read(|c| get(c,"Job",id)?.context("Missing job"))
}

#[derive(Default)]
struct Calls { active: usize, peak: usize, started: HashMap<String,usize>, by_job: HashMap<String,usize> }

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::current_exe()?.file_name().is_some_and(|name| name == "report-agent.exe") {
        let result = synthetic_agent().await;
        if let Err(error) = &result { eprintln!("Synthetic Agent: {error:#}"); }
        // Tokio stdin uses a blocking reader; this child intentionally keeps stdin open.
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/acceptance/tauri-slots");
    let root = base.join(common::id());
    let resources = root.join("resources");
    std::fs::create_dir_all(resources.join("agent"))?;
    std::fs::copy(std::env::current_exe()?,resources.join("agent/report-agent.exe"))?;
    let calls = Arc::new(Mutex::new(Calls::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let model_port = listener.local_addr()?.port();
    let (release, released) = tokio::sync::watch::channel(false);
    let server_calls = calls.clone();
    let server = tokio::spawn(async move {
        loop {
            let (socket,_) = listener.accept().await?;
            let mut released = released.clone();
            let calls = server_calls.clone();
            tokio::spawn(async move {
                let mut socket = BufReader::new(socket);
                let mut jid = String::new();
                socket.read_line(&mut jid).await?;
                let jid = jid.trim().to_owned();
                {
                    let mut counts = calls.lock().unwrap();
                    counts.active += 1;
                    counts.peak = counts.peak.max(counts.active);
                    *counts.started.entry(jid.clone()).or_default() += 1;
                    *counts.by_job.entry(jid.clone()).or_default() += 1;
                }
                let mut byte = [0u8;1];
                if !*released.borrow() {
                    tokio::select! {
                        _ = socket.read(&mut byte) => {},
                        _ = released.changed() => {},
                    }
                }
                {
                    let mut counts = calls.lock().unwrap();
                    counts.active -= 1;
                    *counts.by_job.get_mut(&jid).unwrap() -= 1;
                }
                if *released.borrow() { let _ = socket.get_mut().write_all(b"x").await; }
                Ok::<_,anyhow::Error>(())
            });
        }
        #[allow(unreachable_code)] Ok::<_,anyhow::Error>(())
    });
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let leader = Leader::new(root.join("Leader"),resources)?;
    leader.dispatch("leader.configure",json!({"settings":{"id":common::id(),"name":"并发槽位验证","timeZone":"Asia/Shanghai","port":port,"addressMode":"manual","advertisedUrl":format!("https://127.0.0.1:{port}"),"apiBaseUrl":format!("http://127.0.0.1:{model_port}/v1"),"model":"fixture"},"apiKey":"fixture-key"})).await?;
    let day = common::today("Asia/Shanghai")?;
    let mut jobs = Vec::new();
    for name in ["成员甲","成员乙"] {
        let member = leader.dispatch("leader.addMember",json!({"name":name})).await?;
        let mid = member["id"].as_str().unwrap();
        let sid = common::id();
        let capture = json!({"day":day,"timeZone":"Asia/Shanghai","capturedAt":common::now(),"sessionCount":1,"warnings":[],"events":[{"id":common::id(),"sessionId":"synthetic","timestamp":common::now(),"kind":"user","text":"隔离的并发与取消验证。","source":"sessions/sample.jsonl:1"}]});
        let text = serde_json::to_string(&capture)?;
        let filename = format!("{sid}.json.gz");
        let mut gzip = flate2::write::GzEncoder::new(std::fs::File::create(leader.store.root.join("submissions").join(&filename))?,flate2::Compression::default());
        gzip.write_all(text.as_bytes())?; gzip.finish()?;
        leader.store.write(|c|put(c,"Snapshot",&sid,&json!({"id":sid,"memberId":mid,"day":day,"version":1,"hash":common::hash(&text),"fileName":filename,"receivedAt":common::now(),"capturedAt":capture["capturedAt"],"sessionCount":1,"warnings":[]}),&day))?;
        let response = leader.dispatch("leader.generate",json!({"memberIds":[mid],"day":day})).await?;
        ensure!(response["errors"].as_array().is_some_and(Vec::is_empty), "Could not queue job: {response}");
        let queued = leader.store.read(|c|all(c,"Job",Some(&day))?.into_iter().find(|j|j["memberId"]==mid).context("Missing queued job"))?;
        jobs.push(queued["id"].as_str().unwrap().to_owned());
    }
    leader.clone().start().await?;
    wait_for(|| Ok(calls.lock().unwrap().active == 4 && jobs.iter().all(|id|leader.store.root.join("agent").join(id).join("attempt.marker").exists()))).await?;
    let cancelled = {
        let counts = calls.lock().unwrap();
        jobs.iter().max_by_key(|id|counts.by_job.get(*id).copied().unwrap_or(0)).unwrap().clone()
    };
    let survivor = jobs.iter().find(|id|**id!=cancelled).unwrap().clone();
    let started_cancelled = calls.lock().unwrap().started[&cancelled];
    let start = std::time::Instant::now();
    leader.dispatch("leader.cancel",json!({"jobId":cancelled})).await?;
    wait_for(|| { let counts = calls.lock().unwrap(); Ok(counts.by_job[&cancelled]==0 && counts.by_job.get(&survivor)==Some(&4)) }).await?;
    let cancel_ms = start.elapsed().as_millis();
    release.send_replace(true);
    wait_for(||Ok(job(&leader,&survivor)?["state"]=="failed")).await?;
    ensure!(calls.lock().unwrap().started[&survivor]==6,"Surviving first attempt did not finish all six requests");
    let failed = job(&leader,&survivor)?;
    ensure!(failed["progress"]["total"]==6 && failed["progress"]["completed"]==6 && failed["progress"]["active"]==0,"Failed progress lost segmentation counts");
    leader.dispatch("leader.retry",json!({"jobId":survivor})).await?;
    wait_for(||Ok(job(&leader,&survivor)?["state"]=="completed")).await?;
    let completed = job(&leader,&survivor)?;
    let report = leader.store.read(|c|get(c,"Report",completed["reportId"].as_str().unwrap())?.context("Missing final report"))?;
    ensure!(completed["progress"]["total"]==6 && completed["progress"]["completed"]==6 && completed["progress"]["estimatedTokens"]==123456 && completed["progress"]["retries"]==1,"Completion lost structured progress");
    ensure!(report["analysisSources"]["event-1:0-100"]=="会话第一段原文" && report["analysisCoverage"]["complete"]==true,"Evidence did not reach report");
    ensure!(report["sources"]==json!({}),"Detailed event evidence leaked into PDF source table");
    ensure!(job(&leader,&cancelled)?["state"]=="cancelled","Cancelled state changed");
    ensure!(leader.store.read(|c|Ok(all(c,"Report",Some(&day))?.len()))?==1,"Cancelled or failed partial report was published");
    wait_for(||Ok(calls.lock().unwrap().active==0)).await?;
    let result = {
        let counts = calls.lock().unwrap();
        ensure!(counts.peak==4,"Global four-request limit exceeded: {}",counts.peak);
        ensure!(counts.started[&cancelled]==started_cancelled,"Cancelled waiting requests started later");
        ensure!(counts.started[&survivor]==12,"Failed/retried requests did not release all slots");
        json!({"ok":true,"root":root,"globalPeak":counts.peak,"cancelMilliseconds":cancel_ms,"cancelledRequests":started_cancelled,"survivorRequests":counts.started[&survivor],"completedProgress":completed["progress"],"verified":["two real child processes share four request slots","cancelling occupied and waiting requests releases slots","other job continues; failure and same-ID retry return slots","cancelled and failed work does not publish reports","chunk progress and separate analysis evidence persist"],"model":"synthetic Agent protocol; local TCP requests only"})
    };
    leader.shutdown().await;
    server.abort();
    common::atomic_text(&base.join("latest-result.json"),&serde_json::to_string_pretty(&result)?)?;
    println!("Shared request-slot workflow passed: {}",base.display());
    Ok(())
}
