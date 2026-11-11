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
