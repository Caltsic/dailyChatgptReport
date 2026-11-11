use super::*;
use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

impl Leader {
    pub(super) fn tls_identity(&self) -> Result<(native_tls::Identity, String)> {
        let mut cached = self.certificate.lock().unwrap();
        if let Some(v) = cached.as_ref() {
            return Ok(v.clone());
        }
        let path = self.store.root.join("certificate.secret");
        let pfx = if path.exists() {
            STANDARD.decode(common::read_secret(&path)?)?
        } else {
            let machine =
                std::env::var("COMPUTERNAME").unwrap_or_else(|_| "CodexDepartment".into());
            let key = rcgen::KeyPair::generate()?;
            let mut params = rcgen::CertificateParams::new(vec![
                "localhost".to_owned(),
                machine,
                "127.0.0.1".to_owned(),
            ])?;
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, "Codex Department Daily Report");
            params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
            params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
            let cert = params.self_signed(&key)?;
            let bundle = p12::PFX::new(
                cert.der(),
                &key.serialize_der(),
                None,
                "",
                "Codex Department Daily Report",
            )
            .ok_or_else(|| anyhow!("无法创建部门 TLS 证书"))?;
            let bytes = bundle.to_der();
            common::save_secret(&path, &STANDARD.encode(&bytes))?;
            bytes
        };
        let identity = native_tls::Identity::from_pkcs12(&pfx, "")?;
        let fingerprint = pfx_fingerprint(&pfx)?;
        *cached = Some((identity.clone(), fingerprint.clone()));
        Ok((identity, fingerprint))
    }
    pub(super) async fn restart_listener(&self) -> Result<()> {
        let mut handle = self.listener.lock().await;
        if let Some(old) = handle.take() {
            old.abort();
            let _ = old.await;
        }
        let result:Result<tokio::task::JoinHandle<()>>=async {
            let cfg=self.settings()?;
            let socket=tokio::net::TcpListener::bind(("0.0.0.0",n(&cfg,"port") as u16)).await?;
            let tls=tokio_native_tls::TlsAcceptor::from(native_tls::TlsAcceptor::builder(self.tls_identity()?.0).build()?);
            let me=self.self_ref.get().and_then(|w|w.upgrade()).ok_or_else(||anyhow!("服务状态无效"))?;
            let mut stopping=self.stop.subscribe();
            Ok(tokio::spawn(async move {
                let mut clients=tokio::task::JoinSet::new();
                loop {
                    if *stopping.borrow(){break;}
                    tokio::select! {
                        _=stopping.changed()=>break,
                        next=socket.accept()=>{
                            match next {
                                Ok((stream,peer))=>{
                                    let tls=tls.clone(); let state=me.clone();
                                    clients.spawn(async move {
                                        let Ok(Ok(stream))=tokio::time::timeout(std::time::Duration::from_secs(15),tls.accept(stream)).await else{return;};
                                        let app=Router::new().fallback(http_request).with_state((state,peer.ip().to_string()));
                                        let svc=hyper_util::service::TowerToHyperService::new(app);
                                        let builder=hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
                                        let _=builder.serve_connection(hyper_util::rt::TokioIo::new(stream),svc).await;
                                    });
                                },
                                Err(e)=>{*me.service_error.lock().unwrap()=Some(format!("部门监听失败：{e}"));break;}
                            }
                        },
                        _=clients.join_next(),if !clients.is_empty()=>{}
                    }
                }
                clients.shutdown().await;
            }))
        }.await;
        match result {
            Ok(task) => {
                *handle = Some(task);
                *self.service_error.lock().unwrap() = None;
                Ok(())
            }
            Err(e) => {
                *self.service_error.lock().unwrap() = Some(e.to_string());
                Err(e)
            }
        }
    }
    pub(super) async fn worker(self: Arc<Self>) {
        let mut last_backup_check = std::time::Instant::now() - std::time::Duration::from_secs(61);
        loop {
            let jobs = self.store.write(|c| {
                let mut queued = all(c, "Job", None)?
                    .into_iter()
                    .filter(|v| s(v, "state") == "queued")
                    .collect::<Vec<_>>();
                for j in &mut queued {
                    j["state"] = json!("running");
                    j["error"] = Value::Null;
                    put(c, "Job", s(j, "id"), j, s(j, "day"))?;
                }
                Ok(queued)
            });
            if let Ok(jobs) = jobs {
                for j in jobs {
                    let id = s(&j, "id").to_owned();
                    // Register while holding this lock so a concurrent cancellation either
                    // prevents startup or obtains the task handle and waits for its teardown.
                    let mut tasks = self.job_tasks.lock().unwrap();
                    if !self.store.read(|c| Ok(get(c, "Job", &id)?.is_some_and(|job| s(&job, "state") == "running"))).unwrap_or(false) {
                        continue;
                    }
                    let me = self.clone();
                    let task_id = id.clone();
                    let task = tokio::spawn(async move {
                        if let Err(e) = me.process_job(j).await {
                            let message = common::redact(&e.to_string());
                            let _ = me.store.write(|c| {
                                if let Some(mut job) = get(c, "Job", &task_id)? {
                                    if s(&job, "state") == "running" {
                                        job["state"] = json!(if *me.stop.borrow() {
                                            "queued"
                                        } else {
                                            "failed"
                                        });
                                        job["error"] = if *me.stop.borrow() {
                                            Value::Null
                                        } else {
                                            json!(message)
                                        };
                                        progress(&mut job, "生成失败", &message, false);
                                        job["progress"]["active"] = json!(0);
                                        job["progress"]["requestStartedAt"] = Value::Null;
                                        put(c, "Job", &task_id, &job, s(&job, "day"))?;
                                    }
                                }
                                Ok(())
                            });
                        }
                        let mut tasks = me.job_tasks.lock().unwrap();
                        if tasks.get(&task_id).is_some_and(|task| task.id() == tokio::task::id()) {
                            tasks.remove(&task_id);
                        }
                    });
                    tasks.insert(id, task);
                }
            }
            if last_backup_check.elapsed() > std::time::Duration::from_secs(60) {
                last_backup_check = std::time::Instant::now();
                if let Ok(cfg) = self.settings() {
                    if let Ok(day) = common::today(s(&cfg, "timeZone")) {
                        let marker = self.store.root.join("last-backup.txt");
                        if std::fs::read_to_string(&marker).unwrap_or_default() != day {
                            let me = self.clone();
                            let _ = tokio::task::spawn_blocking(move || {
                                me.backup(&me.store.root.join("backups"))?;
                                common::atomic_text(&marker, &day)
                            })
                            .await;
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
    pub(super) async fn cancel_job(&self, id: &str) -> Result<Value> {
        let job = self.store.write(|c| {
            let mut job = get(c, "Job", id)?.ok_or_else(|| anyhow!("任务不存在"))?;
            if pending(&job) {
                job["state"] = json!("cancelled");
                job["error"] = Value::Null;
                job["finishedAt"] = json!(common::now());
                progress(&mut job, "已取消", "组长取消了本次生成，可重新生成", false);
                job["progress"]["requestStartedAt"] = Value::Null;
                job["progress"]["active"] = json!(0);
                put(c, "Job", id, &job, s(&job, "day"))?;
                audit(c, "取消生成任务", id)?;
            }
            Ok(job)
        })?;
        if s(&job, "state") == "cancelled" {
            let task = self.job_tasks.lock().unwrap().remove(id);
            if let Some(task) = task {
                task.abort();
                // Dropping the Agent future closes its Windows process group and HTTP stream.
                let _ = task.await;
            }
        }
        Ok(job)
    }
    fn track(&self, id: &str, stage: &str, message: &str) -> Result<()> {
        self.store.write(|c| {
            if let Some(mut j) = get(c, "Job", id)? {
                if s(&j, "state") == "running" {
                    progress(&mut j, stage, message, false);
                    put(c, "Job", id, &j, s(&j, "day"))?;
                }
            }
            Ok(())
        })
    }
    fn track_agent(&self, id: &str, event: &Value) -> Result<()> {
        self.store.write(|c| {
            if let Some(mut job) = get(c, "Job", id)? {
                if s(&job, "state") == "running" {
                    if s(event, "type") == "evidence" {
                        job["analysisSources"] = event["sources"].clone();
                        job["analysisCoverage"] = event["coverage"].clone();
                    } else {
                        progress(&mut job, s(event, "stage"), &common::redact(s(event, "message")), false);
                        for field in ["completed", "total", "active", "failed", "pieces", "estimatedTokens", "requests", "retries", "current", "requestStartedAt"] {
                            if let Some(value) = event.get(field) {
                                job["progress"][field] = value.clone();
                            }
                        }
                    }
                    put(c, "Job", id, &job, s(&job, "day"))?;
                }
            }
            Ok(())
        })
    }
    async fn process_job(&self, job: Value) -> Result<()> {
        let jid = s(&job, "id");
        let day = s(&job, "day");
        self.track(jid, "准备分析", "开始或继续当前 Agent 任务")?;
        let cfg = self.settings()?;
        if s(&job, "kind") == "personal" {
            let (snap, m) = self.store.read(|c| {
                Ok((
                    get(c, "Snapshot", s(&job, "snapshotId"))?
                        .ok_or_else(|| anyhow!("来源记录不存在"))?,
                    member(c, s(&job, "memberId"))?,
                ))
            })?;
            let cap = self.load_capture(&snap)?;
            self.track(
                jid,
                "个人日报 Agent",
                &format!(
                    "{} 个会话 · {} 条记录",
                    n(&cap, "sessionCount"),
                    cap["events"].as_array().map_or(0, Vec::len)
                ),
            )?;
            let input = json!({"department":cfg["name"],"employee":m["name"],"day":day,"timeZone":cap["timeZone"],"capturedAt":cap["capturedAt"],"sessionCount":cap["sessionCount"],"collectionNotes":cap["warnings"],"events":cap["events"]});
            let body = self
                .agent(jid, "personal", &common::prompt("personal")?, input)
                .await?;
            self.store.write(|c| {
                let Some(job) = get(c, "Job", jid)?.filter(|j| s(j, "state") == "running") else { return Ok(()); };
                let id = common::id();
                let version = latest(c, "Report", s(&m, "id"), day)?.map_or(0, |r| n(&r, "version")) + 1;
                let report = json!({"id":id,"memberId":m["id"],"memberName":m["name"],"department":cfg["name"],"day":day,"version":version,"snapshotId":snap["id"],"snapshotVersion":snap["version"],"capturedAt":cap["capturedAt"],"sessionCount":cap["sessionCount"],"warnings":cap["warnings"],"content":{"tasks":[],"outputs":[],"verification":[],"blockers":[],"nextSteps":[]},"body":body,"sources":{},"analysisSources":job["analysisSources"],"analysisCoverage":job["analysisCoverage"],"promptVersion":"personal.v2","model":cfg["model"],"createdAt":common::now(),"releasedAt":common::now(),"finalizedAt":null,"feedback":null,"timeZone":cap["timeZone"]});
                put(c, "Report", &id, &report, day)?;
                complete(c, jid, &id)?;
                audit(c, "生成并下发个人日报", &id)
            })
        } else {
            let path = self
                .store
                .root
                .join("agent")
                .join(jid)
                .join("department-input.json");
            let input: Value = if path.exists() {
                serde_json::from_slice(&std::fs::read(&path)?)?
            } else {
                let v = self.store.read(|c| self.summary_input(c, day))?;
                common::atomic_text(&path, &serde_json::to_string(&v)?)?;
                v
            };
            let reports = input["reports"]
                .as_array()
                .ok_or_else(|| anyhow!("汇总输入无效"))?;
            ensure!(!reports.is_empty(), "尚无已提交补充说明的员工日报");
            self.track(
                jid,
                "部门汇总 Agent",
                &format!("合并阅读 {} 份已复核日报", reports.len()),
            )?;
            let args = json!({"department":input["department"],"day":day,"reports":reports.iter().map(|r|json!({"id":r["id"],"memberName":r["memberName"],"version":r["version"],"body":r["body"].as_str().map(str::to_owned).unwrap_or_else(||legacy_body(r)),"supplement":r["feedback"]})).collect::<Vec<_>>(),"missingMembers":input["missing"]});
            let body = self
                .agent(jid, "department", &common::prompt("department")?, args)
                .await?;
            self.store.write(|c| {
                let Some(job) = get(c, "Job", jid)?.filter(|j| s(j, "state") == "running") else { return Ok(()); };
                let id = common::id();
                let mut sources = serde_json::Map::new();
                for report in reports {
                    sources.insert(s(report, "id").to_owned(), json!(format!("{}　{}　V{}", s(report, "memberName"), s(report, "day"), n(report, "version"))));
                }
                let report = json!({"id":id,"day":day,"version":all(c,"DepartmentReport",Some(day))?.len()+1,"department":input["department"],"content":{"overview":[],"members":[],"blockers":[],"nextSteps":[]},"body":body,"sources":sources,"analysisSources":job["analysisSources"],"analysisCoverage":job["analysisCoverage"],"missingMembers":input["missing"],"sourceSignature":input["signature"],"promptVersion":"department.v2","model":cfg["model"],"createdAt":common::now()});
                put(c, "DepartmentReport", &id, &report, day)?;
                complete(c, jid, &id)?;
                audit(c, "生成部门汇总", &id)
            })
        }
    }
    pub(super) async fn agent(
        &self,
        jid: &str,
        kind: &str,
        prompt: &str,
        input: Value,
    ) -> Result<String> {
        ensure!(valid_id(jid), "任务编号无效");
        ensure!(
            !*self.stop.borrow(),
            "执行已暂停，启动后继续当前 Agent 任务"
        );
        let cfg = self.settings()?;
        ensure!(
            !s(&cfg, "apiBaseUrl").trim().is_empty() && !s(&cfg, "model").trim().is_empty(),
            "请配置模型地址和模型名称"
        );
        let key = common::read_secret(&self.store.root.join("api-key.secret"))
            .map_err(|_| anyhow!("尚未配置 API Key"))?;
        ensure!(!key.trim().is_empty(), "尚未配置 API Key");
        let extract_prompt = common::prompt("extract")?;
        let merge_prompt = common::prompt("merge")?;
        // These owners precede the process guard so cancellation kills the Agent
        // process group before its outstanding model-request permits are returned.
        let mut pending_slots: tokio::task::JoinSet<(String, std::result::Result<tokio::sync::OwnedSemaphorePermit, tokio::sync::AcquireError>)> = tokio::task::JoinSet::new();
        let mut acquired_slots = HashMap::new();
        let mut slot_ids = HashSet::new();
        let mut cmd = self.agent_command()?;
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env("PYTHONIOENCODING", "utf-8")
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            cmd.creation_flags(0x08000000);
        }
        let mut child = cmd.spawn()?;
        let _job_guard = ProcessJob::attach(&child)?;
        let mut stopping = self.stop.subscribe();
        ensure!(!*stopping.borrow(), "执行已暂停，启动后继续当前 Agent 任务");
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let drain = tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            while let Ok(count) = stderr.read(&mut buf).await {
                if count == 0 {
                    break;
                }
            }
        });
        let request = json!({
            "jobId":jid,"kind":kind,"systemPrompt":prompt,"input":input,
            "prompts":{"extract":extract_prompt,"merge":merge_prompt},"slotProtocol":true,
            "api":{"baseUrl":cfg["apiBaseUrl"],"key":key,"model":cfg["model"],
                "contextTokens":cfg["contextTokens"].as_u64().unwrap_or(65_536),
                "outputReserveTokens":cfg["outputReserveTokens"].as_u64().unwrap_or(8_192),
                "chunkTokens":cfg["chunkTokens"].as_u64().unwrap_or(48_000)},
            "workspace":self.store.root.join("agent").join(jid)
        });
        let mut line = serde_json::to_vec(&request)?;
        line.push(b'\n');
        stdin.write_all(&line).await?;
        let mut lines = BufReader::new(stdout).lines();
        let mut result = None;
        let mut failure = None;
        loop {
            let line = tokio::select! {
                line = lines.next_line() => line?,
                _ = stopping.changed() => { let _ = child.kill().await; bail!("执行已暂停，启动后继续当前 Agent 任务"); }
                grant = pending_slots.join_next(), if !pending_slots.is_empty() => {
                    let (request_id, permit) = grant.ok_or_else(|| anyhow!("请求队列已中断"))??;
                    acquired_slots.insert(request_id.clone(), permit?);
                    let grant = json!({"type":"slot","requestId":request_id});
                    let mut grant = serde_json::to_vec(&grant)?;
                    grant.push(b'\n');
                    stdin.write_all(&grant).await?;
                    continue;
                }
            };
            let Some(line) = line else {
                break;
            };
            if line.trim().is_empty() {
                continue;
            }
            let event: Value =
                serde_json::from_str(&line).map_err(|_| anyhow!("报告 Agent 返回了无效响应"))?;
            match s(&event, "type") {
                "request_slot" => {
                    let request_id = s(&event, "requestId").to_owned();
                    ensure!(!request_id.is_empty() && slot_ids.insert(request_id.clone()), "报告 Agent 请求编号无效或重复");
                    let slots = self.request_slots.clone();
                    // Do not wait here: stdout must keep draining release_slot events.
                    pending_slots.spawn(async move { (request_id, slots.acquire_owned().await) });
                }
                "release_slot" => {
                    let request_id = s(&event, "requestId");
                    acquired_slots.remove(request_id);
                    slot_ids.remove(request_id);
                }
                "result" => result = Some(s(&event, "text").to_owned()),
                "error" => {
                    failure = Some(common::redact(
                        &s(&event, "message")
                            .replace(&key, "[密钥已隐藏]")
                            .replace(prompt, "[系统提示词已隐藏]")
                            .replace(&extract_prompt, "[系统提示词已隐藏]")
                            .replace(&merge_prompt, "[系统提示词已隐藏]"),
                    ))
                }
                "progress" | "evidence" => self.track_agent(jid, &event)?,
                _ => {}
            }
        }
        let status = child.wait().await?;
        let _ = drain.await;
        if let Some(error) = failure {
            bail!("{error}");
        }
        ensure!(
            status.success(),
            "报告 Agent 退出（代码 {:?}），请重试当前任务",
            status.code()
        );
        result
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("模型未返回报告正文，请重试当前任务"))
    }
    fn agent_command(&self) -> Result<Command> {
        for folder in [&self.resource_dir, &self.resource_dir.join("resources")] {
            let packaged = folder.join("agent").join("report-agent.exe");
            if packaged.exists() {
                return Ok(Command::new(packaged));
            }
        }
        let mut roots = vec![self.resource_dir.clone()];
        if let Ok(exe) = std::env::current_exe() {
            if let Some(p) = exe.parent() {
                roots.push(p.to_path_buf());
            }
        }
        for root in roots {
            for folder in root.ancestors() {
                let py = folder.join(".tools/agent-venv/Scripts/python.exe");
                let script = folder.join("agent/runner.py");
                if py.exists() && script.exists() {
                    let mut cmd = Command::new(py);
                    cmd.arg(script);
                    return Ok(cmd);
                }
            }
        }
        bail!("缺少报告 Agent 运行时，请使用完整组长端安装包")
    }
}
fn progress(j: &mut Value, stage: &str, message: &str, done: bool) {
    if j["progress"].is_null() {
        j["progress"] = json!({"stage":"准备分析","completed":0,"total":0,"current":"","activity":"准备分析","startedAt":common::now(),"updatedAt":common::now(),"requestStartedAt":null,"requests":0,"retries":0,"repairs":0,"events":[]});
    }
    let p = &mut j["progress"];
    p["stage"] = json!(stage);
    p["activity"] = json!(message);
    p["updatedAt"] = json!(common::now());
    if done {
        if n(p, "total") == 0 {
            p["completed"] = json!(1);
            p["total"] = json!(1);
        }
        p["active"] = json!(0);
        p["requestStartedAt"] = Value::Null;
    }
    let mut events = p["events"].as_array().cloned().unwrap_or_default();
    if events.len() >= 100 {
        events.drain(..events.len() - 99);
    }
    events.push(json!({"at":common::now(),"message":message}));
    p["events"] = json!(events);
}
fn complete(c: &Connection, id: &str, report_id: &str) -> Result<()> {
    let mut j = get(c, "Job", id)?.ok_or_else(|| anyhow!("任务不存在"))?;
    j["state"] = json!("completed");
    j["error"] = Value::Null;
    j["reportId"] = json!(report_id);
    let message = if s(&j, "kind") == "personal" { "报告已自动下发，等待成员同步" } else { "部门汇总已生成，可直接预览" };
    progress(&mut j, "生成完成", message, true);
    put(c, "Job", id, &j, s(&j, "day"))
}
fn legacy_body(r: &Value) -> String {
    let mut lines = vec![];
    for (k, title) in [
        ("tasks", "当日任务"),
        ("outputs", "实际执行与产出"),
        ("verification", "验证情况"),
        ("blockers", "未完成事项与阻塞"),
        ("nextSteps", "后续计划"),
    ] {
        lines.push(title.to_owned());
        if let Some(items) = r["content"][k].as_array() {
            for item in items {
                lines.push(format!("【{}】{}", s(item, "status"), s(item, "text")));
            }
        }
    }
    lines.join("\n")
}
async fn http_request(
    State((leader, peer)): State<(Arc<Leader>, String)>,
    request: Request,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let is_pair = path == "/v1/pair";
    if is_pair {
        let mut limits = leader.pair_limits.lock().unwrap();
        limits.retain(|_, (t, _)| t.elapsed().as_secs() < 60);
        let row = limits.entry(peer).or_insert((std::time::Instant::now(), 0));
        row.1 += 1;
        if row.1 > 15 {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "配对请求过于频繁，请稍后重试",
            );
        }
    }
    let mid = if is_pair {
        String::new()
    } else {
        let token = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        match leader.authenticate(token) {
            Ok(id) => id,
            Err(e) => return error(StatusCode::UNAUTHORIZED, &e.to_string()),
        }
    };
    let bytes = match axum::body::to_bytes(request.into_body(), 64 * 1024 * 1024).await {
        Ok(v) => v,
        Err(_) => return error(StatusCode::BAD_REQUEST, "请求格式或大小不受支持"),
    };
    let result: Result<Response> = (|| {
        let data = || -> Result<Value> { Ok(serde_json::from_slice(&bytes)?) };
        let value = match (method.as_str(), path.as_str()) {
            ("POST", "/v1/pair") => leader.pair(&data()?)?,
            ("POST", "/v1/submissions") => leader.submit(&mid, &bytes)?,
            ("POST", "/v1/supplements") => leader.supplement(&mid, &data()?)?,
            ("GET", p) if p.starts_with("/v1/status/") => leader.status(&mid, &p[11..])?,
            ("GET", p) if p.starts_with("/v1/reports/") && p.ends_with("/pdf") => {
                let rid = &p[12..p.len() - 4];
                let r = leader.store.read(|c| {
                    get(c, "Report", rid)?.ok_or_else(|| anyhow!("正式报告不存在或尚未定稿"))
                })?;
                ensure!(
                    s(&r, "memberId") == mid
                        && !r["releasedAt"].is_null()
                        && !r["finalizedAt"].is_null(),
                    "正式报告不存在或尚未定稿"
                );
                ensure!(valid_id(rid), "报告编号无效");
                let dir = leader.store.root.join("reports");
                let mut file = dir.join(format!("{rid}-V{}.pdf", n(&r, "version")));
                if !file.exists() {
                    let digest = common::hash(&dotnet_json(&r)?);
                    file = dir.join(format!("{rid}-{}.pdf", &digest[..16]));
                    ensure!(file.is_file(), "归档文件缺失，请联系组长");
                }
                let mut response = Response::new(Body::from(std::fs::read(file)?));
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("application/pdf"),
                );
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static("no-store"),
                );
                return Ok(response);
            }
            _ => return Ok(error(StatusCode::NOT_FOUND, "接口不存在")),
        };
        let mut response = axum::Json(value).into_response();
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
        Ok(response)
    })();
    match result {
        Ok(v) => v,
        Err(e) => {
            let message = e.to_string();
            if e.is::<std::io::Error>() || e.is::<rusqlite::Error>() {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "部门端处理失败，请稍后重试或联系组长",
                );
            }
            let status = if e.is::<serde_json::Error>()
                || e.is::<chrono::ParseError>()
                || [
                    "配对请求无效",
                    "提交格式无效",
                    "内容校验失败",
                    "日期格式错误",
                    "协议、日期或部门时区不匹配",
                    "采集范围无效或过大",
                    "记录包含无效事件",
                    "记录计数或警告格式错误",
                    "缺少记录",
                    "缺少采集说明",
                ]
                .contains(&message.as_str())
            {
                StatusCode::BAD_REQUEST
            } else if is_pair {
                StatusCode::UNAUTHORIZED
            } else if [
                "报告不存在",
                "正式报告不存在或尚未定稿",
                "归档文件缺失，请联系组长",
                "成员不存在",
            ]
            .contains(&message.as_str())
            {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::CONFLICT
            };
            error(status, &message)
        }
    }
}
fn error(code: StatusCode, message: &str) -> Response {
    let mut response = (code, axum::Json(json!({"error":message}))).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
fn pfx_fingerprint(bytes: &[u8]) -> Result<String> {
    #[repr(C)]
    struct Blob {
        len: u32,
        ptr: *const u8,
    }
    #[repr(C)]
    struct Context {
        encoding: u32,
        data: *const u8,
        len: u32,
        info: *const std::ffi::c_void,
        store: *mut std::ffi::c_void,
    }
    #[link(name = "crypt32")]
    extern "system" {
        fn PFXImportCertStore(
            blob: *const Blob,
            password: *const u16,
            flags: u32,
        ) -> *mut std::ffi::c_void;
        fn CertEnumCertificatesInStore(
            store: *mut std::ffi::c_void,
            previous: *const Context,
        ) -> *const Context;
        fn CertFreeCertificateContext(context: *const Context) -> i32;
        fn CertCloseStore(store: *mut std::ffi::c_void, flags: u32) -> i32;
    }
    unsafe {
        let blob = Blob {
            len: bytes.len() as u32,
            ptr: bytes.as_ptr(),
        };
        let password = [0u16];
        let store = PFXImportCertStore(&blob, password.as_ptr(), 0x8000);
        ensure!(!store.is_null(), "无法读取部门证书");
        let cert = CertEnumCertificatesInStore(store, std::ptr::null());
        if cert.is_null() {
            CertCloseStore(store, 0);
            bail!("部门证书内容无效");
        }
        use sha2::Digest;
        let result = format!(
            "{:X}",
            sha2::Sha256::digest(std::slice::from_raw_parts(
                (*cert).data,
                (*cert).len as usize
            ))
        );
        CertFreeCertificateContext(cert);
        CertCloseStore(store, 0);
        Ok(result)
    }
}

// A job owns the entire Python process tree. Windows closes it even after an abrupt desktop exit.
struct ProcessJob(isize);
unsafe impl Send for ProcessJob {}
impl ProcessJob {
    fn attach(child: &tokio::process::Child) -> Result<Self> {
        #[repr(C)]
        #[derive(Default)]
        struct Basic {
            process_time: i64,
            job_time: i64,
            flags: u32,
            min_working: usize,
            max_working: usize,
            active: u32,
            affinity: usize,
            priority: u32,
            scheduling: u32,
        }
        #[repr(C)]
        #[derive(Default)]
        struct Extended {
            basic: Basic,
            io: [u64; 6],
            process_memory: usize,
            job_memory: usize,
            peak_process: usize,
            peak_job: usize,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn CreateJobObjectW(attributes: *const std::ffi::c_void, name: *const u16) -> isize;
            fn SetInformationJobObject(
                job: isize,
                class: u32,
                data: *const std::ffi::c_void,
                len: u32,
            ) -> i32;
            fn AssignProcessToJobObject(job: isize, process: isize) -> i32;
        }
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            ensure!(
                handle != 0,
                "无法创建 Agent 进程组：{}",
                std::io::Error::last_os_error()
            );
            let owner = Self(handle);
            let mut limits = Extended::default();
            limits.basic.flags = 0x2000;
            ensure!(
                SetInformationJobObject(
                    handle,
                    9,
                    &limits as *const _ as *const _,
                    std::mem::size_of::<Extended>() as u32
                ) != 0,
                "无法管理 Agent 进程生命周期：{}",
                std::io::Error::last_os_error()
            );
            let process = child
                .raw_handle()
                .ok_or_else(|| anyhow!("Agent 进程已退出"))?;
            ensure!(
                AssignProcessToJobObject(handle, process as isize) != 0,
                "无法关联 Agent 进程组：{}",
                std::io::Error::last_os_error()
            );
            Ok(owner)
        }
    }
}
impl Drop for ProcessJob {
    fn drop(&mut self) {
        #[link(name = "kernel32")]
        extern "system" {
            fn CloseHandle(handle: isize) -> i32;
        }
        unsafe {
            CloseHandle(self.0);
        }
    }
}
