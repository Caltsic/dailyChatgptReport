// Isolated multi-source acceptance: Codex + DeepSeek Harness collection, session
// selection, actual HTTPS upload and Leader snapshot validation.
mod support;
use anyhow::{ensure, Context, Result};
use daily_report::{common, leader::Leader, member::MemberClient};
use serde_json::{json, Value};
use std::{fs, io::Read, path::{Path, PathBuf}};

fn now_ms() -> i64 {
    common::now()
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap()
        .timestamp_millis()
}

fn codex_fixture(root: &Path) -> Result<()> {
    fs::create_dir_all(root.join("sessions"))?;
    let lines = [
        json!({"type":"session_meta","payload":{"id":"codex-demo","title":"Codex 会话","cwd":"C:\\Workspace\\Demo"}}),
        json!({"timestamp":common::now(),"type":"response_item","payload":{"type":"message","role":"user",
            "content":[{"type":"input_text","text":"CODEX_USER_VISIBLE 整理导出功能"}]}}),
        json!({"timestamp":common::now(),"type":"response_item","payload":{"type":"message","role":"assistant","channel":"final",
            "content":[{"type":"output_text","text":"CODEX_ASSISTANT_VISIBLE 已完成列名调整"}]}}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    fs::write(root.join("sessions/demo.jsonl"), format!("{lines}\n"))?;
    Ok(())
}

fn dsh_fixture(root: &Path) -> Result<()> {
    let directory = root.join("sessions/--F-WORKSPACE-Demo--/session-demo");
    fs::create_dir_all(&directory)?;
    let now = now_ms();
    let rows = [
        json!({"type":"session","version":4,"id":"session-demo","createdAt":now-60_000,
            "cwd":"F:\\WORKSPACE\\Demo","isSeeded":false,"delegationDepth":0,"agentPreset":"standard"}),
        json!({"type":"user/message","seq":1,"time":now-9_000,
            "data":{"content":[{"type":"text","text":"DSH_USER_VISIBLE 补充接口超时排查"}],
                "source":{"kind":"user"},"role":"user","id":"m1"}}),
        json!({"type":"user/message","seq":2,"time":now-8_500,
            "data":{"content":[{"type":"text","text":"DSH_INJECTED_CONTEXT_MUST_NOT_APPEAR"}],
                "source":{"kind":"runtime-context"},"role":"user","id":"m2"}}),
        json!({"type":"system/message","seq":3,"time":now-8_000,
            "data":{"turn":1,"step":1,"message":{"role":"system",
                "content":[{"type":"text","text":"DSH_SYSTEM_PROMPT_MUST_NOT_APPEAR"}],
                "source":{"kind":"system-prompt"},"id":"s1"}}}),
        json!({"type":"request/header","seq":4,"time":now-7_500,
            "data":{"header":{"config":{"provider":"test","model":"test"}}}}),
        json!({"type":"assistant/message","seq":5,"time":now-7_000,
            "data":{"turn":1,"step":1,"message":{"role":"assistant","content":[
                {"type":"reasoning","text":"DSH_REASONING_MUST_NOT_APPEAR"},
                {"type":"text","text":"DSH_ASSISTANT_VISIBLE 已定位下游等待"}]}}}),
        json!({"type":"tool/call","seq":6,"time":now-6_000,
            "data":{"turn":1,"step":1,"callId":"call-1","name":"pwsh","arguments":"{\"command\":\"grep timeout app.log\"}"}}),
        json!({"type":"tool/result","seq":7,"time":now-5_000,
            "data":{"turn":1,"step":1,"message":{"role":"tool","source":{"kind":"tool","callId":"call-1"},
                "toolCallId":"call-1","content":[{"type":"text","text":"DSH_TOOL_RESULT_VISIBLE 命中 3 处超时"}],"isError":false}}}),
        json!({"type":"session/title","seq":8,"time":now-4_000,
            "data":{"title":"DSH 接口超时排查","source":{"kind":"user"}}}),
    ];
    let mut bytes = Vec::new();
    for chunk in [&rows[..1], &rows[1..]] {
        let payload = format!(
            "{}\n",
            chunk.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")
        );
        bytes.extend(zstd::encode_all(payload.as_bytes(), 3)?);
    }
    fs::write(directory.join("session.v4.jsonl.zstd"), bytes)?;
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
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/acceptance/multi-source");
    let root = base.join(chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string());
    let codex_root = root.join("source-codex");
    let dsh_root = root.join("source-dsh");
    codex_fixture(&codex_root)?;
    dsh_fixture(&dsh_root)?;
    let day = common::today("Asia/Shanghai")?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let leader = Leader::new(root.join("Leader"), root.clone())?;
    leader.dispatch("leader.configure", json!({"settings":{"id":common::id(),"name":"多来源验证部门","timeZone":"Asia/Shanghai",
        "port":port,"addressMode":"manual","advertisedUrl":format!("https://127.0.0.1:{port}"),"apiBaseUrl":"","model":""},"apiKey":""})).await?;
    let employee = leader.dispatch("leader.addMember", json!({"name":"多来源验证成员"})).await?;
    leader.clone().start().await?;
    let invitation = leader.dispatch("leader.invite", json!({"memberId":employee["id"]})).await?;
    let member = MemberClient::new(root.join("Member"))?;
    member.dispatch("member.pair", json!({"invitation":invitation["invitation"]})).await?;
    let configured = member.dispatch("member.sources", json!({"sources":[
        {"tool":"codex","enabled":true,"path":codex_root.to_string_lossy()},
        {"tool":"dsh","enabled":true,"path":dsh_root.to_string_lossy()}
    ]})).await?;
    ensure!(configured["sources"].as_array().unwrap().iter()
        .all(|source| source["enabled"] == true && source["detected"] == true), "Source configuration not saved");

    let draft = member.dispatch("member.collect", json!({"day":day})).await?;
    let sessions = draft["sessions"].as_array().context("Missing sessions")?;
    ensure!(sessions.len() == 2, "Expected one session per source: {sessions:?}");
    let codex_session = sessions.iter().find(|session| session["tool"] == "codex").context("Missing Codex session")?;
    let dsh_session = sessions.iter().find(|session| session["tool"] == "dsh").context("Missing DSH session")?;
    ensure!(codex_session["id"] == "codex-demo", "Codex session id changed");
    ensure!(dsh_session["id"] == "dsh:session-demo", "DSH session id not namespaced");
    ensure!(dsh_session["title"] == "DSH 接口超时排查", "DSH session title missing");
    ensure!(dsh_session["project"] == "F:\\WORKSPACE\\Demo", "DSH project missing");
    ensure!(dsh_session["eventCount"] == 4, "DSH visible event count wrong: {dsh_session}");
    ensure!(codex_session["eventCount"] == 2, "Codex event count wrong");

    let preview = member.dispatch("member.previewSession",
        json!({"draftId":draft["draftId"],"sessionId":dsh_session["id"]})).await?;
    let preview_text = preview.to_string();
    for visible in ["DSH_USER_VISIBLE", "DSH_ASSISTANT_VISIBLE", "DSH_TOOL_RESULT_VISIBLE"] {
        ensure!(preview_text.contains(visible), "Preview missing visible content: {visible}");
    }
    for internal in ["DSH_SYSTEM_PROMPT_MUST_NOT_APPEAR", "DSH_REASONING_MUST_NOT_APPEAR",
        "DSH_INJECTED_CONTEXT_MUST_NOT_APPEAR"] {
        ensure!(!preview_text.contains(internal), "Preview contains internal record: {internal}");
    }
    ensure!(preview["events"].as_array().unwrap().iter().all(|event| event["tool"] == "dsh"),
        "DSH preview event tool label missing");

    let selected = sessions.iter().map(|session| session["id"].clone()).collect::<Vec<_>>();
    let submitted = member.dispatch("member.submit",
        json!({"draftId":draft["draftId"],"sessionIds":selected})).await?;
    ensure!(submitted["received"] == true, "Leader did not receive multi-source upload: {submitted}");
    ensure!(submitted["sessionCount"] == 2 && submitted["eventCount"] == 6, "Submitted counts wrong: {submitted}");

    let uploaded = capture(&leader, &day, 1)?;
    ensure!(uploaded["protocol"] == 1, "Protocol changed");
    ensure!(uploaded["sessionCount"] == 2, "Snapshot session count wrong");
    let events = uploaded["events"].as_array().context("Missing events")?;
    ensure!(events.len() == 6, "Snapshot event count wrong: {}", events.len());
    ensure!(events.iter().any(|event| event["sessionId"] == "dsh:session-demo" && event["tool"] == "dsh"),
        "Snapshot missing DSH event");
    ensure!(events.iter().all(|event| ["user", "assistant", "tool_call", "tool_result"].contains(&event["kind"].as_str().unwrap())),
        "Snapshot event kind invalid");
    let sources = uploaded["sources"].as_array().context("Snapshot missing source summary")?;
    ensure!(sources.iter().any(|source| source["tool"] == "codex") && sources.iter().any(|source| source["tool"] == "dsh"),
        "Snapshot source summary wrong");
    let serialized = uploaded.to_string();
    for internal in ["DSH_SYSTEM_PROMPT_MUST_NOT_APPEAR", "DSH_REASONING_MUST_NOT_APPEAR",
        "DSH_INJECTED_CONTEXT_MUST_NOT_APPEAR", "request/header"] {
        ensure!(!serialized.contains(internal), "Snapshot contains internal record: {internal}");
    }
    ensure!(member.dispatch("member.init", json!({})).await?["sources"].as_array().unwrap().len() == 2,
        "Restart lost source configuration");

    leader.shutdown().await;
    let result = json!({"ok":true,"root":root,"day":day,"sources":configured,"draft":draft,
        "preview":preview,"submitted":submitted,"uploaded":uploaded,
        "verified":["enabled sources configured separately","Codex and DeepSeek Harness sessions collected together",
            "DSH internal records excluded before upload","session title/project/tool labels preserved",
            "actual HTTPS upload accepted by Leader validation","snapshot keeps both sources and namespaced ids"],
        "modelCalled":false});
    common::atomic_text(&base.join("latest-result.json"), &serde_json::to_string_pretty(&result)?)?;
    println!("Multi-source upload workflow passed: {}", base.display());
    Ok(())
}
