#![allow(dead_code)]

use anyhow::Result;
use daily_report::member::MemberClient;
use serde_json::{json, Value};

// Acceptance examples confirm the selections returned by the same local collection flow as the UI.
pub async fn submit_selected(member: &MemberClient, day: &str) -> Result<Value> {
    let draft = member.dispatch("member.collect", json!({"day":day})).await?;
    let ids = draft["sessions"].as_array().unwrap().iter().filter(|s|s["selected"] == true)
        .map(|s|s["id"].clone()).collect::<Vec<_>>();
    member.dispatch("member.submit", json!({"draftId":draft["draftId"],"sessionIds":ids})).await
}

// Every acceptance fixture pins the Codex root and disables other sources so a developer
// machine with real DeepSeek Harness records cannot leak into the isolated run.
pub async fn codex_only(member: &MemberClient, root: &std::path::Path) -> Result<()> {
    member
        .dispatch(
            "member.sources",
            json!({"sources":[
                {"tool":"codex","enabled":true,"path":root.to_string_lossy()},
                {"tool":"dsh","enabled":false}
            ]}),
        )
        .await?;
    Ok(())
}
