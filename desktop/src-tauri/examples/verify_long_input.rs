// Run the already submitted failing snapshot in an isolated department. No collection or production writes.
use anyhow::{ensure, Context, Result};
use daily_report::{common::{self, all, get, put}, leader::Leader};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::{path::PathBuf, time::{Duration, Instant}};

#[tokio::main]
async fn main() -> Result<()> {
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize()?;
    let root = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| project.join("artifacts/live/segmented-035").join(chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string()));
    let leader = Leader::new(root.join("Leader"), project.clone())?;
    let meta_path = root.join("run.json");
    let meta: Value = if meta_path.exists() {
        serde_json::from_slice(&std::fs::read(&meta_path)?)?
    } else {
        let source = PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA missing")?).join("CodexDailyReport/Leader");
        let db = Connection::open_with_flags(source.join("department.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let source_snapshot = "60efde312b324926848a5cebc7a95908";
        let mut snap = get(&db, "Snapshot", source_snapshot)?.context("Specified failing snapshot missing")?;
        let mut settings = get(&db, "DepartmentSettings", "settings")?.context("Model settings missing")?;
        let source_member = get(&db, "Member", snap["memberId"].as_str().context("Member missing")?)?.context("Member missing")?;
        let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        settings["port"] = json!(port);
        settings["addressMode"] = json!("manual");
        settings["advertisedUrl"] = json!(format!("https://127.0.0.1:{port}"));
        let key = common::read_secret(&source.join("api-key.secret"))?;
        leader.dispatch("leader.configure", json!({"settings":settings,"apiKey":key})).await?;
        let member = leader.dispatch("leader.addMember", json!({"name":source_member["name"]})).await?;
        snap["memberId"] = member["id"].clone();
        let filename = snap["fileName"].as_str().context("Capture file missing")?;
        std::fs::copy(source.join("submissions").join(filename), leader.store.root.join("submissions").join(filename))?;
        leader.store.write(|c| put(c,"Snapshot",source_snapshot,&snap,snap["day"].as_str().unwrap()))?;
        let meta = json!({"root":root,"memberId":member["id"],"day":snap["day"],"sourceSnapshot":source_snapshot,"sourceHash":snap["hash"],"scope":"isolated actual submitted sample; no production writes or employee confirmation"});
        common::atomic_text(&meta_path,&serde_json::to_string_pretty(&meta)?)?;
        meta
    };
    let day = meta["day"].as_str().context("Day missing")?;
    let jobs = leader.store.read(|c| all(c,"Job",Some(day)))?;
    let regenerate = std::env::args().any(|arg|arg == "--regenerate");
    if regenerate {
        let previous = jobs.last().context("Previous isolated task missing")?;
        ensure!(previous["state"] == "completed", "Previous run must be complete before comparison");
        let response = leader.dispatch("leader.generate",json!({"memberIds":[meta["memberId"]],"day":day})).await?;
        ensure!(response["errors"].as_array().is_some_and(Vec::is_empty),"Could not queue comparison: {response}");
        let jobs = leader.store.read(|c|all(c,"Job",Some(day)))?;
        let next = jobs.last().context("Comparison task missing")?;
        // Extraction/merge prompts and input are unchanged: retain validated evidence, rerun final reasoning.
        let old_cache = leader.store.root.join("agent").join(previous["id"].as_str().unwrap()).join("pipeline");
        let new_cache = leader.store.root.join("agent").join(next["id"].as_str().unwrap()).join("pipeline");
        for namespace in std::fs::read_dir(old_cache)? {
            let namespace = namespace?;
            let results = namespace.path().join("results");
            let destination = new_cache.join(namespace.file_name()).join("results");
            std::fs::create_dir_all(&destination)?;
            for result in std::fs::read_dir(results)? {
                let result = result?;
                std::fs::copy(result.path(), destination.join(result.file_name()))?;
            }
        }
    } else if let Some(job) = jobs.last() {
        if job["state"] == "failed" { leader.dispatch("leader.retry",json!({"jobId":job["id"]})).await?; }
        ensure!(job["state"] != "cancelled", "The isolated run was cancelled");
    } else {
        let response = leader.dispatch("leader.generate",json!({"memberIds":[meta["memberId"]],"day":day})).await?;
        ensure!(response["errors"].as_array().is_some_and(Vec::is_empty),"Generation could not queue: {response}");
    }
    leader.clone().start().await?;
    println!("Isolated long-input run: {}",root.display());
    let started = Instant::now();
    let mut last_line = String::new();
    let job = loop {
        let dashboard = leader.dispatch("leader.dashboard",json!({"day":day})).await?;
        let job = dashboard["jobs"].as_array().and_then(|jobs|jobs.first()).context("Job missing")?.clone();
        let p = &job["progress"];
        let line = json!({"state":job["state"],"stage":p["stage"],"completed":p["completed"],"total":p["total"],"active":p["active"],"failed":p["failed"],"requests":p["requests"],"message":p["activity"]}).to_string();
        if line != last_line {println!("{}s {line}",started.elapsed().as_secs()); last_line=line;}
        if matches!(job["state"].as_str(),Some("completed"|"failed"|"cancelled")) {break job;}
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    leader.shutdown().await;
    common::atomic_text(&root.join("job.json"),&serde_json::to_string_pretty(&job)?)?;
    ensure!(job["state"] == "completed","Long-input generation failed: {}",job["error"]);
    let report = leader.store.read(|c| get(c,"Report",job["reportId"].as_str().context("Report ID missing")?)?.context("Report missing"))?;
    common::atomic_text(&root.join("personal.json"),&serde_json::to_string_pretty(&report)?)?;
    common::atomic_text(&root.join("personal.md"),report["body"].as_str().context("Body missing")?)?;
    println!("Completed in {} seconds; inspect {}",started.elapsed().as_secs(),root.join("personal.md").display());
    Ok(())
}
