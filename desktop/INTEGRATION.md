# Tauri migration contract

Version 0.3.6 (Member and Leader). Preserve existing `%LOCALAPPDATA%/CodexDailyReport/{Member,Leader}` JSON/SQLite entity names and wire v1. Rust owns desktop/business; Python LangGraph stays leader-only. No .NET/Chromium/Playwright in new packages. No new generic frameworks.

`common.rs` (root agent): `pub type Result<T> = anyhow::Result<T>`, `id()->String`, `now()->String` RFC3339 UTC, `hash(&str)->String` uppercase SHA256, `today(&str)->Result<String>`, `atomic_text(&Path,&str)->Result<()>`, `save_secret(&Path,&str)->Result<()>`, `read_secret(&Path)->Result<String>`, `protect(&str)->Result<String>` / `unprotect(&str)->Result<String>` base64 DPAPI, `redact(&str)->String`. Also `Store::new(root:PathBuf)->Result<Store>`, public `root:PathBuf`, `read(|&Connection| -> Result<T>)`, `write(|&Connection| -> Result<T>)`; free `get(conn,kind,id)->Result<Option<Value>>`, `all(conn,kind,Option<&str>)->Result<Vec<Value>>`, `put(conn,kind,id,&Value,day)->Result<()>`, `delete(conn,kind,id)->Result<()>`. JSON existing database uses camelCase and kind names e.g. Member, Report, Job.

`member.rs` + `member_capture.rs` + `collector.rs` (member agent): `MemberClient::new(root:PathBuf)->Result<Arc<Self>>`; `start(self:Arc<Self>)` spawns background flush every 10 sec; `async dispatch(&self,op:&str,args:Value)->Result<Value>`. Operations:
- member.init -> {pairing:null|Pairing,codexRoot,queue:[]}
- member.pair {invitation:base64String} -> Pairing (UI displays invitation consent first)
- member.status {day} -> {status:null|MemberStatus,queue:[],syncMessage:string,draft:string,feedbackBlockedReason:string}; flush before fetch; cached status on offline with syncMessage. Local feedbackBlockedReason derives from existing report/receipt IDs and the legacy waiting status; requires no Leader update. A stale report stays readable and its draft editable, while submit waits for a current report.
- member.collect {day} -> {draftId,day,timeZone,capturedAt,sessions:[{id,title,project,firstAt,lastAt,eventCount,warnings,selected,isNew}],warnings}; collect a redacted local snapshot without uploading
- member.previewSession {draftId,sessionId} -> {events:[{id,sessionId,timestamp,kind,text,source}],warnings}; exact complete snapshot content
- member.saveSelection {draftId,sessionIds:[]} -> null; persist choices for department/member/day/timezone/canonical log root; initially select all, subsequently new sessions start unselected
- member.submit {draftId,sessionIds:[]} -> {capturedAt,sessionCount,eventCount,warnings,received,syncMessage}; filter the existing snapshot, recompute count/hash, durably enqueue then flush; only server receipt means successful. Repeated confirmation reuses the same queue item. Changing an already confirmed selection requires a new collection. No reread on confirm or retry; removed old member.submit {day} operation
- member.draft {reportId,day,text} save -> null
- member.feedback {reportId,text,noSupplement} -> {syncMessage:string} (queue then flush). Capture reportId/text before awaiting UI draft persistence. Sending a click snapshot must not overwrite a newer draft. A specific HTTP 409 stale-version rejection retains the encrypted original request in feedback-conflicts and preserves its draft; it is not resent or reassigned to a new version. Other failures keep existing queue semantics.
- member.codexRoot {path} -> null
- member.download {reportId,path} -> {path}
- member.retry {id} -> null
Preserve pairing.secret and outbox/drafts existing formats. Selection memory uses DPAPI-protected session-selection/<context-hash>.secret; unconfirmed capture content remains in memory. Unselected session content/title/project/warnings stay local. Global collection warnings remain in the selected payload. Explicit zero selection carries a scope note distinct from no records found, compatible with Leader 0.3.2 warning classification. No agent on member.

`leader.rs` (leader agent): `Leader::new(root:PathBuf,resource_dir:PathBuf)->Result<Arc<Self>>`; `async start(self:Arc<Self>)->Result<()>`; `async dispatch(&self,op:&str,args:Value)->Result<Value>`. Own HTTPS server, durable jobs, existing agent protocol, backup. Dependencies supplied by root; report requirements preserve current C# behavior (feedback required for export).
- leader.init -> {settings:null|DepartmentSettings,fingerprint:string,serviceError:string|null,networkAddresses:{name,address}[],networkError:string|null}; refresh legacy localhost or automatic advertised URL
- leader.networkAddresses -> {addresses:{name,address}[]}; enumerate connected Windows IPv4 adapters, physical LAN first
- leader.configure {settings:DepartmentSettings,apiKey:string} -> null (retain identity; restart listener if changed). Optional settings.addressMode: auto/manual; automatic mode resolves current IP, manual preserves the configured address. Listener binds all IPv4 interfaces.
- leader.testConnection -> {message:string}
- Capacity planning settings: `contextTokens=65536`, `outputReserveTokens=8192`, `chunkTokens=48000`. These estimate request capacity; no provider output-length limit is set. Missing fields in existing departments use these defaults.
- leader.dashboard {day} -> {rows:DashboardRow[],jobs:Job[],summary:null|DepartmentReport,summaryStale:bool}
- Completed jobs include reportId pointing to their exact generated personal/department report. Older jobs may omit it; label preview as current version. Personal generation sets releasedAt automatically in the same transaction; preview never requires feedback/finalization.
- leader.addMember {name} -> Member
- leader.invite {memberId} -> {invitation:base64String,details:Invitation}
- leader.revoke {memberIds:[]} -> null
- leader.generate / leader.release / leader.reopen {memberIds:[],day} -> {errors:[]}; empty means all active members
- leader.resubmit {memberIds:[],day,reason} -> {errors:[]}
- leader.history {memberId,day} -> Report[] newest first
- leader.retry {jobId} -> null
- leader.cancel {jobId} -> Job; queued/running becomes cancelled with finishedAt, waits for active task teardown and closes the Agent process group. Completed/failed/cancelled jobs return their existing state. Progress/failure/report commits cannot overwrite cancellation. Cancelled jobs survive restart; generate/summary creates a fresh job. Other jobs continue.
- leader.summary {day} -> Job
- leader.prepareExport {reportId} -> Report with finalizedAt proposed (does not persist)
- leader.commitExport {report:Report} -> null (verify version/source/feedback still current)
- leader.backup {path?} -> {path}
- leader.restore {path,target} -> {path} restore to new directory only
Leader HTTPS v1 routes same as C#; archived PDF under existing reports naming (coordinate root docs).

Long-input Agent protocol (0.3.5): the first stdin line includes `slotProtocol:true`, encrypted-resource prompts `{extract,merge}`, and the capacity settings in `api`. Host keeps stdin open. Before each HTTP attempt the Agent emits `{type:request_slot,requestId}`; host asynchronously waits for a permit from the Leader-wide semaphore of four, then writes `{type:slot,requestId}`. Agent emits `release_slot` after that attempt. Reading stdout must continue while grants wait, so releases cannot deadlock. Dropping a task kills its process tree before returning active permits. Connection tests use the same request pool.

Agent progress carries `stage,message,completed,total,active,failed,pieces,estimatedTokens,requests,retries`; each piece has `id,label,state,error?`. Host stores these in Job.progress. An `evidence` event stores `sources,coverage` as Job/Report `analysisSources,analysisCoverage`. PDF `sources` remains a compact report-level index; it must not be filled with all event ranges. Successful piece results and LangGraph checkpoints live below `agent/<jobId>/pipeline/<content-config-hash>`. Final reports stay Markdown; internal evidence JSON is not the final report format.

Long-input graph: plan → parallel extract → session aggregate → reconcile → write. Reconciliation compares the original extracted facts against the compressed baseline in capacity-sized batches; if the original facts fit, the writer uses them directly. `recordedTimeRange` comes from source-event timestamps normalized to UTC and indicates evidence order, not work duration. Baseline plus the largest indivisible fact must fit the review input; compress the baseline first when necessary. Review results use the same source validation, cache, request slots, and retry protocol. Nested model agents set `checkpointer=False` so an incomplete model response cannot be reused through the outer graph checkpoint.

`retrying` is a waiting piece state, not an active request. Rate-limit backoff starts at 10 seconds and increases to at most 60 seconds; a short provider Retry-After does not reduce that local delay. Request permits are released while waiting. Success caches survive failed tasks, while cancellation still follows the fresh-job rule above.

Root Tauri router `dispatch(op,args)` forwards prefix. `app.info` -> {role:'Member'|'Leader',version,day,root}; `app.openPath {path}` opens generated file/directory; `app.exportPersonal {reportId,directory}` prepares, writes PDF and commits; `app.exportSummary {reportId,directory}` exports the selected summary with stale check. Both return {pdf,directory}. Native WebView2 PDF uses the existing HTML template; no Word generation, archive requirement or copy remains. Existing PDFs with matching content signatures can be reused without a Word file. Historical Word files are not deleted. Frontend uses `@tauri-apps/api/core.invoke('dispatch',{op,args})`; folder/file dialogs `@tauri-apps/plugin-dialog`.

UI agent owns `desktop/ui`, `desktop/index.html`, `desktop/package.json`, `desktop/tsconfig.json`, `desktop/vite.config.ts` only. Root owns src-tauri boot/Cargo/common/documents/build scripts. Member agent owns member.rs/collector.rs. Leader agent owns leader.rs (can split leader_*.rs). No changes to live data/installed apps during verification.
