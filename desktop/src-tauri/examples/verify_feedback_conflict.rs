// One HTTPS flow: stale report -> rejected supplement retained -> latest report confirmation.
use anyhow::{ensure, Result};
use daily_report::{common::{self, get, put}, leader::Leader, member::MemberClient};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

#[tokio::main]
async fn main() -> Result<()> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/acceptance/feedback-conflict");
    let root = base.join(common::id());
    let member_root = root.join("Member");
    let leader_root = root.join("Leader");
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let day = "2026-11-09";
    let leader = Leader::new(leader_root, root.clone())?;
    leader.dispatch("leader.configure", json!({"settings":{
        "id":common::id(),"name":"补充版本验证","timeZone":"Asia/Shanghai","addressMode":"manual",
        "port":port,"advertisedUrl":format!("https://127.0.0.1:{port}"),"apiBaseUrl":"http://127.0.0.1:1","model":"no-model"
    },"apiKey":""})).await?;
    let employee = leader.dispatch("leader.addMember", json!({"name":"样例成员"})).await?;
    let mid = employee["id"].as_str().unwrap();
    let invite = leader.dispatch("leader.invite", json!({"memberId":mid})).await?;
    let member = MemberClient::new(member_root.clone())?;
    member.dispatch("member.pair", json!({"invitation":invite["invitation"]})).await?;
    let sid1 = common::id();
    let sid2 = common::id();
    let rid1 = common::id();
    let rid2 = common::id();
    let snap = |sid: &str, version: i32| json!({"id":sid,"memberId":mid,"day":day,"version":version,"receivedAt":common::now(),"capturedAt":common::now(),"sessionCount":1,"warnings":[]});
    let report = |rid: &str, sid: &str, version: i32| json!({"id":rid,"memberId":mid,"memberName":"样例成员","department":"补充版本验证","day":day,"version":version,"snapshotId":sid,"snapshotVersion":version,"capturedAt":common::now(),"sessionCount":1,"warnings":[],"body":format!("## 样例正文 V{version}\n用于版本冲突验证"),"content":{},"sources":{},"createdAt":common::now(),"releasedAt":common::now(),"feedback":null,"finalizedAt":null,"timeZone":"Asia/Shanghai"});
    leader.store.write(|c| { put(c,"Snapshot",&sid1,&snap(&sid1,1),day)?; put(c,"Report",&rid1,&report(&rid1,&sid1,1),day) })?;
    let initial = member.dispatch("member.status",json!({"day":day})).await?;
    ensure!(initial["feedbackBlockedReason"] == "", "Current report is blocked");
    let supplement = "实际补充原文：保留换行\n尚需现场核对。";
    leader.store.write(|c| put(c,"Snapshot",&sid2,&snap(&sid2,2),day))?;
    // The member still has the previous cached report when the server receives this request.
    let rejected = member.dispatch("member.feedback",json!({"reportId":rid1,"text":supplement,"noSupplement":false})).await?;
    ensure!(rejected["syncMessage"].as_str().unwrap_or("").contains("刚才的补充未提交"),"Conflict was reported as success");
    let stale = member.dispatch("member.status",json!({"day":day})).await?;
    ensure!(stale["feedbackBlockedReason"].as_str().unwrap_or("").contains("请组长生成新版"),"Missing stale-source explanation");
    ensure!(stale["draft"] == supplement && stale["queue"].as_array().unwrap().is_empty(),"Draft lost or rejected request remains queued");
    let archived = fs::read_dir(member_root.join("feedback-conflicts"))?.next().unwrap()?.path();
    let record: Value = serde_json::from_str(&common::read_secret(&archived)?)?;
    let payload: Value = serde_json::from_str(record["payload"].as_str().unwrap())?;
    ensure!(payload["text"] == supplement && payload["reportId"] == rid1,"Rejected submission changed");
    // A retained 0.3.3 conflict must be migrated without replacing a newer editable draft.
    let old_id = common::id();
    let mut old_record = record.clone();
    old_record["id"] = json!(old_id);
    common::save_secret(&member_root.join("outbox").join(format!("{old_id}.secret")), &old_record.to_string())?;
    let newer_draft = "用户继续编辑的新草稿";
    member.dispatch("member.draft",json!({"reportId":rid1,"text":newer_draft})).await?;
    let migrated = member.dispatch("member.status",json!({"day":day})).await?;
    ensure!(migrated["draft"] == newer_draft && migrated["queue"].as_array().unwrap().is_empty(),"Old queue recovery overwrote draft");
    member.dispatch("member.feedback",json!({"reportId":rid1,"text":supplement,"noSupplement":false})).await?;
    let preserved = member.dispatch("member.status",json!({"day":day})).await?;
    ensure!(preserved["draft"] == newer_draft,"Sending an earlier click snapshot replaced newer edits");
    leader.store.write(|c| put(c,"Report",&rid2,&report(&rid2,&sid2,2),day))?;
    let fresh = member.dispatch("member.status",json!({"day":day})).await?;
    ensure!(fresh["status"]["report"]["id"] == rid2 && fresh["draft"] == newer_draft && fresh["draftFromPrevious"] == true && fresh["feedbackBlockedReason"] == "","New version did not restore draft for review");
    ensure!(fresh["status"]["report"]["feedback"].is_null(),"Draft was auto-submitted");
    member.dispatch("member.feedback",json!({"reportId":rid2,"text":newer_draft,"noSupplement":false})).await?;
    leader.store.read(|c| {
        ensure!(get(c,"Report",&rid1)?.unwrap()["feedback"].is_null(),"Old report was modified");
        ensure!(get(c,"Report",&rid2)?.unwrap()["feedback"]["text"] == newer_draft,"Latest confirmation missing");
        Ok(())
    })?;
    let result = json!({"passed":true,"root":root,"scope":"actual MemberClient HTTPS requests; isolated report/source fixtures; no model call", "checks":["stale source status","409 retained as exact encrypted record and draft","old rejected queue migration preserves newer draft","latest report draft needs explicit confirmation","feedback attached only to selected report"],"initial":initial,"stale":stale,"fresh":fresh});
    common::atomic_text(&base.join("result.json"),&serde_json::to_string_pretty(&result)?)?;
    leader.shutdown().await;
    println!("PASS member HTTPS version conflict, draft recovery and explicit latest-version feedback");
    Ok(())
}
