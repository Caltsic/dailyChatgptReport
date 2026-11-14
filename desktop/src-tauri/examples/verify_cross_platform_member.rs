// Headless Linux member acceptance: pair with a real Leader through a forwarded port,
// collect the enabled local sources, upload and report counts only. Build without the
// desktop shell: `cargo build --no-default-features --example verify_cross_platform_member`.
use daily_report::{common, member::MemberClient};
use serde_json::json;
use std::{fs, path::PathBuf};

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|value| value == name)
        .and_then(|index| args.get(index + 1).cloned())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let invitation_path = PathBuf::from(arg("--invitation").expect("--invitation 必填"));
    let day = arg("--day").expect("--day 必填");
    let out = PathBuf::from(arg("--out").expect("--out 必填"));
    let invitation = fs::read_to_string(&invitation_path)?.trim().to_owned();
    let root = out.parent().unwrap().join("cross-platform-member-data");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root)?;

    let member = MemberClient::new(root.clone())?;
    let paired = member
        .dispatch("member.pair", json!({"invitation": invitation}))
        .await?;
    anyhow::ensure!(
        paired["identity"]["departmentName"].as_str().is_some_and(|name| !name.is_empty()),
        "配对返回缺少部门信息：{paired}"
    );

    let mut sources = Vec::new();
    for (tool, flag) in [("codex", "--codex"), ("dsh", "--dsh"), ("trae", "--trae")] {
        if let Some(path) = arg(flag) {
            sources.push(json!({"tool":tool,"enabled":true,"path":path}));
        }
    }
    anyhow::ensure!(!sources.is_empty(), "至少提供一个 --codex/--dsh/--trae 来源");
    member
        .dispatch("member.sources", json!({"sources": sources}))
        .await?;

    let draft = member.dispatch("member.collect", json!({"day":day})).await?;
    let sessions = draft["sessions"].as_array().cloned().unwrap_or_default();
    let selected = sessions
        .iter()
        .map(|session| session["id"].clone())
        .collect::<Vec<_>>();
    let submitted = member
        .dispatch(
            "member.submit",
            json!({"draftId":draft["draftId"],"sessionIds":selected}),
        )
        .await?;
    anyhow::ensure!(
        submitted["received"] == true,
        "部门端未确认接收：{submitted}"
    );
    let status = member.dispatch("member.status", json!({"day":day})).await?;
    anyhow::ensure!(status["online"] == true, "TLS 状态查询未成功：{status}");
    anyhow::ensure!(
        status["queue"].as_array().map(|queue| queue.is_empty()).unwrap_or(false),
        "上传后队列未清空：{status}"
    );

    // Counts only: this acceptance never writes session text to the result file.
    let summaries = sessions
        .iter()
        .map(|session| {
            json!({"tool":session["tool"],"eventCount":session["eventCount"],
                "warnings":session["warnings"].as_array().map_or(0, Vec::len)})
        })
        .collect::<Vec<_>>();
    let mut by_tool = std::collections::BTreeMap::<String, usize>::new();
    for summary in &summaries {
        *by_tool
            .entry(summary["tool"].as_str().unwrap_or("?").to_owned())
            .or_insert(0) += 1;
    }
    let result = json!({
        "ok":true,
        "day":day,
        "department":paired["identity"]["departmentName"],
        "sessions":summaries,
        "sessionsByTool":by_tool,
        "submitted":submitted,
        "status":{"online":status["online"],"submissionStatus":status["status"]["submissionStatus"],
            "reportStatus":status["status"]["reportStatus"]},
        "verified":[
            "Linux member binary pairs with the real Leader over TLS",
            "certificate fingerprint from the invitation is accepted",
            "enabled local sources are collected on this host",
            "sessions are selected and uploaded over HTTPS",
            "Leader receipt received and queue drained",
            "GET /v1/status reports online"
        ],
        "modelCalled":false
    });
    common::atomic_text(&out, &serde_json::to_string_pretty(&result)?)?;
    println!("cross-platform member upload passed: {}", out.display());
    Ok(())
}
