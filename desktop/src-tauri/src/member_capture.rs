use super::*;
use std::collections::HashSet;

#[derive(Default, Deserialize, Serialize)]
struct SelectionMemory {
    known: HashSet<String>,
    selected: HashSet<String>,
}

pub(super) struct CaptureDraft {
    id: String,
    context: String,
    memory_path: PathBuf,
    memory: SelectionMemory,
    capture: collector::SelectionCapture,
    submitted: Option<SubmittedCapture>,
}

struct SubmittedCapture {
    session_ids: HashSet<String>,
    item_id: String,
    summary: Value,
}

fn context(pairing: &Value, day: &str, root: &Path) -> String {
    let root = root.canonicalize().unwrap_or_else(|_|root.to_owned());
    hash(&json!({"department":pairing["identity"]["departmentId"],"member":pairing["identity"]["memberId"],
        "day":day,"zone":pairing["identity"]["timeZone"],"root":root.to_string_lossy().to_lowercase()}).to_string())
}

impl CaptureDraft {
    fn selection(&self, args: &Value) -> Result<HashSet<String>> {
        let ids = args["sessionIds"].as_array().context("请选择需要上传的会话")?
            .iter().map(|v|v.as_str().map(str::to_owned).context("会话选择格式无效"))
            .collect::<Result<HashSet<_>>>()?;
        let available: HashSet<_> = self.capture.sessions.iter().map(|s|text(s,"id")).collect();
        if ids.iter().any(|id|!available.contains(id.as_str())) { bail!("会话列表已更新，请重新选择"); }
        if self.submitted.as_ref().is_some_and(|s|s.session_ids != ids) {
            bail!("本次采集已确认上传；如需调整范围，请重新采集");
        }
        Ok(ids)
    }

    fn remember(&mut self, ids: &HashSet<String>) -> Result<()> {
        for session in &self.capture.sessions { self.memory.selected.remove(text(session,"id")); }
        self.memory.selected.extend(ids.iter().cloned());
        save_secret(&self.memory_path, &serde_json::to_string(&self.memory)?)
    }

    fn filtered(&self, ids: &HashSet<String>) -> Value {
        let source = &self.capture.capture;
        let events: Vec<_> = source["events"].as_array().unwrap().iter()
            .filter(|e|ids.contains(text(e,"sessionId"))).cloned().collect();
        let count = events.iter().map(|e|text(e,"sessionId")).collect::<HashSet<_>>().len();
        let intentional_empty = ids.is_empty() && !self.capture.sessions.is_empty();
        let mut warnings: Vec<_> = self.capture.global_warnings.iter()
            .filter(|w|!w.starts_with("仅覆盖本机")).cloned().collect();
        for session in &self.capture.sessions {
            if ids.contains(text(session,"id")) {
                warnings.extend(session["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).map(str::to_owned));
            }
        }
        let mut scope = format!("仅覆盖本机所选会话的当日事件；本次报告仅包含员工选择上传的会话；跨日上下文、附件及外部文件未采集；{}。", collector::TRUNCATION_NOTE);
        if intentional_empty { scope.push_str("本次未选择上传会话，不代表当天没有工作。"); }
        else if events.is_empty() { warnings.push("未发现可用工作记录；不代表当天没有工作".to_owned()); }
        warnings.push(scope);
        let mut seen = HashSet::new();
        warnings.retain(|w|seen.insert(w.clone()));
        // Local titles, project paths, excluded sessions and their diagnostics never enter this object.
        json!({"day":source["day"],"timeZone":source["timeZone"],"capturedAt":source["capturedAt"],
            "events":events,"warnings":warnings,"sessionCount":count,"protocol":source["protocol"],
            "sources":source["sources"]})
    }
}

impl MemberClient {
    pub(super) async fn collect_draft(&self, day: &str) -> Result<Value> {
        let pairing = self.require_pairing()?;
        let zone = identity_part(&pairing,"timeZone");
        let roots = self.source_roots()?;
        let key = context(&pairing,day,&self.codex_root());
        if roots.codex.is_none() && roots.dsh.is_none() {
            bail!("尚未启用任何记录来源，请先在“记录来源”中选择要采集的本机目录");
        }
        let memory_path = self.root.join("session-selection").join(format!("{key}.secret"));
        let mut memory: SelectionMemory = if memory_path.exists() {
            serde_json::from_str(&read_secret(&memory_path)?).context("本日会话选择记录无法读取")?
        } else { SelectionMemory::default() };
        let first = !memory_path.exists();
        let mut guard = self.capture.lock().await;
        let day = day.to_owned();
        let capture = tokio::task::spawn_blocking(move || collector::collect_for_selection(&roots,&day,&zone)).await??;
        let mut sessions = capture.sessions.clone();
        for session in &mut sessions {
            let sid = text(session,"id").to_owned();
            let new = !first && !memory.known.contains(&sid);
            if first { memory.selected.insert(sid.clone()); }
            memory.known.insert(sid.clone());
            session["selected"] = json!(memory.selected.contains(&sid));
            session["isNew"] = json!(new);
        }
        save_secret(&memory_path, &serde_json::to_string(&memory)?)?;
        let draft_id = id();
        let response = json!({"draftId":draft_id,"day":capture.capture["day"],"timeZone":capture.capture["timeZone"],
            "capturedAt":capture.capture["capturedAt"],"sessions":sessions,"warnings":capture.global_warnings});
        *guard = Some(CaptureDraft { id:draft_id,context:key,memory_path,memory,capture,submitted:None });
        Ok(response)
    }

    pub(super) async fn preview_session(&self, args: &Value) -> Result<Value> {
        let guard = self.capture.lock().await;
        let draft = guard.as_ref().filter(|d|d.id==text(args,"draftId")).context("采集预览已更新，请重新采集")?;
        let session = draft.capture.sessions.iter().find(|s|text(s,"id")==text(args,"sessionId"))
            .context("会话不在本次采集中")?;
        let events: Vec<_> = draft.capture.capture["events"].as_array().unwrap().iter()
            .filter(|e|e["sessionId"]==session["id"]).collect();
        Ok(json!({"events":events,"warnings":session["warnings"]}))
    }

    pub(super) async fn save_selection(&self, args: &Value) -> Result<Value> {
        let mut guard = self.capture.lock().await;
        let draft = guard.as_mut().filter(|d|d.id==text(args,"draftId")).context("采集预览已更新，请重新采集")?;
        let ids = draft.selection(args)?;
        draft.remember(&ids)?;
        Ok(Value::Null)
    }

    pub(super) async fn submit_selected(&self, args: &Value) -> Result<Value> {
        let mut guard = self.capture.lock().await;
        let draft = guard.as_mut().filter(|d|d.id==text(args,"draftId")).context("请先采集并选择需要上传的会话")?;
        let pairing = self.require_pairing()?;
        if draft.context != context(&pairing,text(&draft.capture.capture,"day"),&self.codex_root()) {
            bail!("部门或记录目录已变更，请重新采集");
        }
        let ids = draft.selection(args)?;
        draft.remember(&ids)?;
        if draft.submitted.is_none() {
            let capture = draft.filtered(&ids);
            let item = self.enqueue("upload", json!({"requestId":id(),"hash":hash(&capture.to_string()),"capture":capture}))?;
            let summary = json!({"capturedAt":capture["capturedAt"],"sessionCount":capture["sessionCount"],
                "eventCount":capture["events"].as_array().unwrap().len(),"warnings":capture["warnings"]});
            draft.submitted = Some(SubmittedCapture {session_ids:ids,item_id:item.id,summary});
        }
        let submission = draft.submitted.as_ref().unwrap();
        let message = self.flush().await?;
        let pending = self.items()?.iter().any(|item|item.id==submission.item_id);
        let mut response = submission.summary.clone();
        response["received"] = json!(!pending);
        response["syncMessage"] = json!(message);
        Ok(response)
    }
}
