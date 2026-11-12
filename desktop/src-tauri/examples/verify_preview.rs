mod support;
// One generation path with a local model response: automatic release and exact job/report association.
use anyhow::{ensure, Context, Result};
use daily_report::{common, leader::Leader, member::MemberClient};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::Duration};

#[tokio::main]
async fn main() -> Result<()> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/acceptance/tauri-preview");
    let root = base.join(chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string());
    let resources = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/tauri-publish/Leader");
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let mock = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let model_port = mock.local_addr()?.port();
    let body = "# 自动下发验证日报\n\n## 当日任务\n\n已检查配对及预览入口。\n\n| 项目 | 结果 |\n| --- | --- |\n| 页面入口 | 已验证 |\n\n## 后续计划\n\n继续由成员复核。";
    let api = axum::Router::new().route("/v1/chat/completions", axum::routing::post(move || {
        let calls = counted.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let chunk = json!({"id":"preview-fixture","object":"chat.completion.chunk","created":1,"model":"fixture",
                "choices":[{"index":0,"delta":{"role":"assistant","content":body},"finish_reason":null}]});
            let end = json!({"id":"preview-fixture","object":"chat.completion.chunk","created":1,"model":"fixture",
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
            ([("content-type", "text/event-stream")], format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"))
        }
    }));
    let server = tokio::spawn(async move { axum::serve(mock, api).await });
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let leader = Leader::new(root.join("Leader"), resources)?;
    leader.dispatch("leader.configure", json!({"settings":{
        "id":common::id(),"name":"预览验证部门","timeZone":"Asia/Shanghai","port":port,
        "addressMode":"manual","advertisedUrl":format!("https://127.0.0.1:{port}"),
        "apiBaseUrl":format!("http://127.0.0.1:{model_port}/v1"),"model":"fixture"
    },"apiKey":"fixture-key"})).await?;
    let employee = leader.dispatch("leader.addMember", json!({"name":"预览验证成员"})).await?;
    let member_id = &employee["id"];
    let invitation = leader.dispatch("leader.invite", json!({"memberId":member_id})).await?;
    let member = MemberClient::new(root.join("Member"))?;
    member.dispatch("member.pair", json!({"invitation":invitation["invitation"]})).await?;
    let logs = root.join("sample-codex/sessions");
    std::fs::create_dir_all(&logs)?;
    std::fs::write(logs.join("sample.jsonl"), format!("{}\n{}\n",
        json!({"type":"session_meta","payload":{"id":"preview-check"}}),
        json!({"timestamp":common::now(),"type":"response_item","payload":{"type":"message","role":"user",
            "content":[{"type":"input_text","text":"软件验证样例：检查自动下发和报告预览。"}]}})))?;
    support::codex_only(&member, &root.join("sample-codex")).await?;
    let day = common::today("Asia/Shanghai")?;
    support::submit_selected(&member, &day).await?;
    leader.clone().start().await?;
    leader.dispatch("leader.generate", json!({"memberIds":[member_id],"day":day})).await?;
    let dashboard = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let dashboard = leader.dispatch("leader.dashboard", json!({"day":day})).await?;
            let job = dashboard["jobs"].as_array().and_then(|jobs|jobs.first()).context("Missing job")?;
            ensure!(job["state"] != "failed", "Generation failed: {}", job["error"]);
            if job["state"] == "completed" { break Ok::<Value, anyhow::Error>(dashboard); }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }).await??;
    let status = member.dispatch("member.status", json!({"day":day})).await?;
    let report = &status["status"]["report"];
    let report_id = report["id"].as_str().context("Generated report was not automatically available to member")?;
    ensure!(report["body"] == body && !report["releasedAt"].is_null(), "Automatic release content mismatch");
    ensure!(dashboard["jobs"][0]["reportId"] == report_id, "Completed task points to wrong report");
    let before_feedback = leader.dispatch("leader.history", json!({"memberId":member_id,"day":day})).await?;
    ensure!(before_feedback[0]["body"] == body && before_feedback[0]["feedback"].is_null()
        && before_feedback[0]["finalizedAt"].is_null(), "Preview requires feedback or finalization");
    let supplement = "成员原文补充：<逐字保留>\n第二行补充说明。";
    member.dispatch("member.feedback", json!({"day":day,"reportId":report_id,"text":supplement,"noSupplement":false})).await?;
    let reports = leader.dispatch("leader.history", json!({"memberId":member_id,"day":day})).await?;
    ensure!(reports[0]["feedback"]["text"] == supplement, "Supplement text changed");
    let settings = leader.dispatch("leader.init", json!({})).await?;
    let member_status = member.dispatch("member.status", json!({"day":day})).await?;
    leader.shutdown().await;
    server.abort();
    ensure!(calls.load(Ordering::SeqCst) == 1, "Unexpected extra model request");
    let result = json!({"ok":true,"root":root,"day":day,"initialization":settings,"dashboard":dashboard,
        "beforeFeedbackReports":before_feedback,"reports":reports,"memberState":member_status,
        "verified":["packaged Agent generation","automatic release without manual operation","exact task report ID",
            "preview before feedback/finalization","verbatim supplement"],"model":"local fixture; one request"});
    common::atomic_text(&base.join("latest-result.json"), &serde_json::to_string_pretty(&result)?)?;
    println!("Automatic release, task/report association and preview data passed: {}", base.display());
    Ok(())
}
