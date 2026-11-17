// Isolated multi-source acceptance: Codex + Claude Code + AGY CLI + DeepSeek Harness + Z Code
// collection, session selection, actual HTTPS upload and Leader snapshot validation.
mod support;
use anyhow::{ensure, Context, Result};
use daily_report::{common, leader::Leader, member::MemberClient};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

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

fn claude_fixture(root: &Path) -> Result<()> {
    let dir = root.join("projects/--F-WORKSPACE-Demo--");
    fs::create_dir_all(&dir)?;
    let lines = [
        json!({"type":"custom-title","sessionId":"claude-demo","customTitle":"Claude 导出联调"}),
        json!({"type":"user","cwd":"F:\\WORKSPACE\\Demo","sessionId":"claude-demo","timestamp":common::now(),
            "message":{"role":"user","content":"CLAUDE_USER_VISIBLE 检查导出字段"}}),
        json!({"type":"user","isMeta":true,"cwd":"F:\\WORKSPACE\\Demo","sessionId":"claude-demo","timestamp":common::now(),
            "message":{"role":"user","content":"CLAUDE_META_MUST_NOT_APPEAR"}}),
        json!({"type":"assistant","cwd":"F:\\WORKSPACE\\Demo","sessionId":"claude-demo","timestamp":common::now(),
            "message":{"role":"assistant","content":[
                {"type":"thinking","thinking":"CLAUDE_THINKING_MUST_NOT_APPEAR"},
                {"type":"text","text":"CLAUDE_ASSISTANT_VISIBLE 字段已核对"}
            ]}}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    fs::write(dir.join("claude-demo.jsonl"), format!("{lines}\n"))?;
    Ok(())
}

fn agy_fixture(root: &Path) -> Result<()> {
    let conv_id = "agy-demo";
    let logs = root
        .join("brain")
        .join(conv_id)
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&logs)?;
    let conn = Connection::open(root.join("conversation_summaries.db"))?;
    conn.execute(
        "CREATE TABLE conversation_summaries (
            conversation_id TEXT PRIMARY KEY,
            title TEXT,
            workspace_uris TEXT,
            parent_conversation_id TEXT
        )",
        [],
    )?;
    conn.execute(
        "INSERT INTO conversation_summaries VALUES (?1, ?2, ?3, '')",
        rusqlite::params![conv_id, "AGY CLI 验证任务", r#"["file:///F:/WORKSPACE/Demo"]"#],
    )?;
    drop(conn);

    let ts = common::now();
    let lines = [
        json!({"step_index":0,"source":"SYSTEM","type":"SYSTEM_MESSAGE","status":"DONE","created_at":ts,"content":"AGY_SYSTEM_MUST_NOT_APPEAR"}),
        json!({"step_index":1,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":ts,
            "content":"<USER_REQUEST>\nAGY_USER_VISIBLE 验证 AGY 采集\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nskip\n</ADDITIONAL_METADATA>"}),
        json!({"step_index":2,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":ts,
            "thinking":"AGY_THINKING_MUST_NOT_APPEAR",
            "content":"AGY_ASSISTANT_VISIBLE 采集完成"}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    fs::write(logs.join("transcript.jsonl"), format!("{lines}\n"))?;
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

fn zcode_fixture(root: &Path) -> Result<()> {
    let db_dir = root.join("cli").join("db");
    fs::create_dir_all(&db_dir)?;
    let now = now_ms();
    let conn = Connection::open(db_dir.join("db.sqlite"))?;
    conn.execute_batch(
        "CREATE TABLE session (
            id text primary key,
            project_id text not null,
            workspace_id text,
            parent_id text,
            slug text not null,
            directory text not null,
            path text,
            title text not null,
            version text not null,
            time_created integer not null,
            time_updated integer not null,
            time_archived integer
        );
        CREATE TABLE message (
            id text primary key,
            session_id text not null,
            time_created integer not null,
            time_updated integer not null,
            data text not null,
            sequence integer
        );
        CREATE TABLE part (
            id text primary key,
            message_id text not null,
            session_id text not null,
            time_created integer not null,
            time_updated integer not null,
            data text not null,
            sequence integer
        );",
    )?;
    conn.execute(
        "INSERT INTO session (id, project_id, parent_id, slug, directory, path, title, version, time_created, time_updated)
         VALUES ('zcode-demo', 'p1', '', 's1', 'F:\\WORKSPACE\\Demo', '', 'Z Code 集成校验', '1', ?1, ?1)",
        rusqlite::params![now - 10_000],
    )?;
    conn.execute(
        "INSERT INTO message VALUES ('zm1', 'zcode-demo', ?1, ?1, ?2, 0)",
        rusqlite::params![now - 9_000, json!({"role":"user"}).to_string()],
    )?;
    conn.execute(
        "INSERT INTO part VALUES ('zp1', 'zm1', 'zcode-demo', ?1, ?1, ?2, 0)",
        rusqlite::params![
            now - 9_000,
            json!({"type":"text","text":"ZCODE_USER_VISIBLE 运行多来源检查"}).to_string()
        ],
    )?;
    conn.execute(
        "INSERT INTO message VALUES ('zm2', 'zcode-demo', ?1, ?1, ?2, 1)",
        rusqlite::params![now - 8_000, json!({"role":"assistant"}).to_string()],
    )?;
    conn.execute(
        "INSERT INTO part VALUES ('zp2', 'zm2', 'zcode-demo', ?1, ?1, ?2, 0)",
        rusqlite::params![
            now - 8_000,
            json!({"type":"reasoning","text":"ZCODE_REASONING_MUST_NOT_APPEAR"}).to_string()
        ],
    )?;
    conn.execute(
        "INSERT INTO part VALUES ('zp3', 'zm2', 'zcode-demo', ?1, ?1, ?2, 1)",
        rusqlite::params![
            now - 7_000,
            json!({"type":"text","text":"ZCODE_ASSISTANT_VISIBLE 检查全部通过"}).to_string()
        ],
    )?;
    Ok(())
}

fn capture(leader: &Leader, day: &str, version: u64) -> Result<Value> {
    let snapshot = leader.store.read(|c| {
        common::all(c, "Snapshot", Some(day))?
            .into_iter()
            .find(|s| s["version"].as_u64() == Some(version))
            .context("Missing snapshot")
    })?;
    let file = fs::File::open(
        leader
            .store
            .root
            .join("submissions")
            .join(snapshot["fileName"].as_str().unwrap()),
    )?;
    let mut text = String::new();
    flate2::read::GzDecoder::new(file).read_to_string(&mut text)?;
    Ok(serde_json::from_str(&text)?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let base =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/acceptance/multi-source");
    let root = base.join(chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string());
    let codex_root = root.join("source-codex");
    let claude_root = root.join("source-claude");
    let agy_root = root.join("source-agy");
    let dsh_root = root.join("source-dsh");
    let zcode_root = root.join("source-zcode");
    codex_fixture(&codex_root)?;
    claude_fixture(&claude_root)?;
    agy_fixture(&agy_root)?;
    dsh_fixture(&dsh_root)?;
    zcode_fixture(&zcode_root)?;

    let day = common::today("Asia/Shanghai")?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let leader = Leader::new(root.join("Leader"), root.clone())?;
    leader
        .dispatch(
            "leader.configure",
            json!({"settings":{"id":common::id(),"name":"多来源验证部门","timeZone":"Asia/Shanghai",
                "port":port,"addressMode":"manual","advertisedUrl":format!("https://127.0.0.1:{port}"),
                "apiBaseUrl":"","model":""},"apiKey":""}),
        )
        .await?;
    let employee = leader
        .dispatch("leader.addMember", json!({"name":"多来源验证成员"}))
        .await?;
    leader.clone().start().await?;
    let invitation = leader
        .dispatch("leader.invite", json!({"memberId":employee["id"]}))
        .await?;
    let member = MemberClient::new(root.join("Member"))?;
    member
        .dispatch("member.pair", json!({"invitation":invitation["invitation"]}))
        .await?;
    let configured = member
        .dispatch(
            "member.sources",
            json!({"sources":[
                {"tool":"codex","enabled":true,"path":codex_root.to_string_lossy()},
                {"tool":"claude","enabled":true,"path":claude_root.to_string_lossy()},
                {"tool":"agy","enabled":true,"path":agy_root.to_string_lossy()},
                {"tool":"dsh","enabled":true,"path":dsh_root.to_string_lossy()},
                {"tool":"zcode","enabled":true,"path":zcode_root.to_string_lossy()}
            ]}),
        )
        .await?;
    ensure!(
        configured["sources"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| matches!(
                s["tool"].as_str(),
                Some("codex" | "claude" | "agy" | "dsh" | "zcode")
            ))
            .all(|s| s["enabled"] == true && s["detected"] == true),
        "Source configuration not saved"
    );

    let draft = member.dispatch("member.collect", json!({"day":day})).await?;
    let sessions = draft["sessions"].as_array().context("Missing sessions")?;
    ensure!(
        sessions.len() == 5,
        "Expected 5 sessions across 5 tools: {sessions:?}"
    );
    let codex_session = sessions
        .iter()
        .find(|s| s["tool"] == "codex")
        .context("Missing Codex session")?;
    let claude_session = sessions
        .iter()
        .find(|s| s["tool"] == "claude")
        .context("Missing Claude session")?;
    let agy_session = sessions
        .iter()
        .find(|s| s["tool"] == "agy")
        .context("Missing AGY session")?;
    let dsh_session = sessions
        .iter()
        .find(|s| s["tool"] == "dsh")
        .context("Missing DSH session")?;
    let zcode_session = sessions
        .iter()
        .find(|s| s["tool"] == "zcode")
        .context("Missing Z Code session")?;
    ensure!(codex_session["id"] == "codex-demo" && codex_session["eventCount"] == 2);
    ensure!(claude_session["id"] == "claude:claude-demo" && claude_session["eventCount"] == 2);
    ensure!(claude_session["title"] == "Claude 导出联调");
    ensure!(agy_session["id"] == "agy:agy-demo" && agy_session["eventCount"] == 2);
    ensure!(agy_session["title"] == "AGY CLI 验证任务");
    ensure!(dsh_session["id"] == "dsh:session-demo" && dsh_session["eventCount"] == 4);
    ensure!(zcode_session["id"] == "zcode:zcode-demo" && zcode_session["eventCount"] == 2);
    ensure!(zcode_session["title"] == "Z Code 集成校验");

    let preview = member
        .dispatch(
            "member.previewSession",
            json!({"draftId":draft["draftId"],"sessionId":dsh_session["id"]}),
        )
        .await?;
    let preview_text = preview.to_string();
    for visible in ["DSH_USER_VISIBLE", "DSH_ASSISTANT_VISIBLE", "DSH_TOOL_RESULT_VISIBLE"] {
        ensure!(
            preview_text.contains(visible),
            "Preview missing visible content: {visible}"
        );
    }

    let selected = sessions
        .iter()
        .map(|s| s["id"].clone())
        .collect::<Vec<_>>();
    let submitted = member
        .dispatch(
            "member.submit",
            json!({"draftId":draft["draftId"],"sessionIds":selected}),
        )
        .await?;
    ensure!(
        submitted["received"] == true,
        "Leader did not receive multi-source upload: {submitted}"
    );
    ensure!(
        submitted["sessionCount"] == 5 && submitted["eventCount"] == 12,
        "Submitted counts wrong: {submitted}"
    );

    let uploaded = capture(&leader, &day, 1)?;
    ensure!(uploaded["protocol"] == 1, "Protocol changed");
    ensure!(uploaded["sessionCount"] == 5, "Snapshot session count wrong");
    let events = uploaded["events"].as_array().context("Missing events")?;
    ensure!(events.len() == 12, "Snapshot event count wrong: {}", events.len());
    ensure!(
        events
            .iter()
            .all(|e| ["user", "assistant", "tool_call", "tool_result"]
                .contains(&e["kind"].as_str().unwrap())),
        "Snapshot event kind invalid"
    );
    let sources = uploaded["sources"]
        .as_array()
        .context("Snapshot missing source summary")?;
    for tool in ["codex", "claude", "agy", "dsh", "zcode"] {
        ensure!(
            sources.iter().any(|s| s["tool"] == tool),
            "Snapshot missing source summary for {tool}"
        );
    }
    let serialized = uploaded.to_string();
    for visible in [
        "CODEX_USER_VISIBLE",
        "CLAUDE_USER_VISIBLE",
        "CLAUDE_ASSISTANT_VISIBLE",
        "AGY_USER_VISIBLE",
        "AGY_ASSISTANT_VISIBLE",
        "DSH_USER_VISIBLE",
        "ZCODE_USER_VISIBLE",
        "ZCODE_ASSISTANT_VISIBLE",
    ] {
        ensure!(
            serialized.contains(visible),
            "Snapshot missing visible record: {visible}"
        );
    }
    for internal in [
        "CLAUDE_META_MUST_NOT_APPEAR",
        "CLAUDE_THINKING_MUST_NOT_APPEAR",
        "AGY_SYSTEM_MUST_NOT_APPEAR",
        "AGY_THINKING_MUST_NOT_APPEAR",
        "DSH_SYSTEM_PROMPT_MUST_NOT_APPEAR",
        "DSH_REASONING_MUST_NOT_APPEAR",
        "DSH_INJECTED_CONTEXT_MUST_NOT_APPEAR",
        "ZCODE_REASONING_MUST_NOT_APPEAR",
        "request/header",
    ] {
        ensure!(
            !serialized.contains(internal),
            "Snapshot contains internal record: {internal}"
        );
    }

    let restored = member.dispatch("member.init", json!({})).await?;
    for tool in ["codex", "claude", "agy", "dsh", "zcode"] {
        ensure!(
            restored["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["tool"] == tool && s["enabled"] == true),
            "Restart lost source configuration for {tool}"
        );
    }

    leader.shutdown().await;
    let result = json!({
        "ok": true,
        "root": root,
        "day": day,
        "sources": configured,
        "draft": draft,
        "preview": preview,
        "submitted": submitted,
        "uploaded": uploaded,
        "verified": [
            "enabled 5 AI sources configured separately",
            "Codex, Claude Code, AGY CLI, DeepSeek Harness, and Z Code collected together",
            "internal thinking/reasoning/system/meta records excluded before upload",
            "session title/project/tool labels preserved",
            "actual HTTPS upload accepted by Leader validation",
            "snapshot keeps all 5 sources and namespaced ids"
        ],
        "modelCalled": false
    });
    common::atomic_text(
        &base.join("latest-result.json"),
        &serde_json::to_string_pretty(&result)?,
    )?;
    println!("Multi-source upload workflow passed: {}", base.display());
    Ok(())
}
