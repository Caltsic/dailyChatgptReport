use crate::common::{self, all, get, put, Result, Store};
use anyhow::{anyhow, bail, ensure};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::Mutex as AsyncMutex;
#[path = "leader_backup.rs"]
mod backup;
#[path = "leader_runtime.rs"]
mod runtime;
#[path = "leader_network.rs"]
mod network;

pub struct Leader {
    pub store: Store,
    stop: tokio::sync::watch::Sender<bool>,
    worker_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    job_tasks: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    request_slots: Arc<tokio::sync::Semaphore>,
    self_ref: std::sync::OnceLock<std::sync::Weak<Leader>>,
    resource_dir: PathBuf,
    listener: AsyncMutex<Option<tokio::task::JoinHandle<()>>>,
    service_error: Mutex<Option<String>>,
    certificate: Mutex<Option<(native_tls::Identity, String)>>,
    pair_limits: Mutex<HashMap<String, (std::time::Instant, u32)>>,
}
pub(super) fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
pub(super) fn n(v: &Value, k: &str) -> i64 {
    v[k].as_i64().unwrap_or(0)
}
pub(super) fn flag(v: &Value, k: &str) -> bool {
    v[k].as_bool().unwrap_or(false)
}
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
fn day_check(day: &str) -> Result<()> {
    ensure!(
        day.len() == 10 && NaiveDate::parse_from_str(day, "%Y-%m-%d").is_ok(),
        "日期格式错误"
    );
    Ok(())
}
fn timestamp(v: &Value, k: &str) -> Result<DateTime<chrono::FixedOffset>> {
    Ok(DateTime::parse_from_rfc3339(s(v, k))?)
}
fn member(c: &Connection, id: &str) -> Result<Value> {
    get(c, "Member", id)?
        .filter(|v| flag(v, "active"))
        .ok_or_else(|| anyhow!("成员不存在"))
}
fn latest(c: &Connection, kind: &str, id: &str, day: &str) -> Result<Option<Value>> {
    Ok(all(c, kind, Some(day))?
        .into_iter()
        .filter(|v| s(v, "memberId") == id)
        .max_by_key(|v| n(v, "version")))
}
fn pending(v: &Value) -> bool {
    matches!(s(v, "state"), "queued" | "running")
}
fn feedback(v: &Value) -> bool {
    !v["feedback"].is_null()
        && (flag(&v["feedback"], "noSupplement") || !s(&v["feedback"], "text").trim().is_empty())
}
fn open(c: &Connection, id: &str, day: &str) -> Result<()> {
    ensure!(
        !get(c, "DayState", &format!("{id}:{day}"))?.is_some_and(|v| flag(&v, "closed")),
        "该日日报已定稿，请组长重新开启后再提交"
    );
    Ok(())
}
fn notices(c: &Connection, id: &str, day: &str) -> Result<bool> {
    Ok(all(c, "Notice", Some(day))?
        .iter()
        .any(|v| s(v, "memberId") == id && !flag(v, "fulfilled")))
}
fn current(c: &Connection, r: &Value) -> Result<()> {
    let m = s(r, "memberId");
    let d = s(r, "day");
    ensure!(
        latest(c, "Report", m, d)?.is_some_and(|x| x["id"] == r["id"])
            && latest(c, "Snapshot", m, d)?.is_some_and(|x| x["id"] == r["snapshotId"]),
        "正文或来源已更新，请使用最新版本"
    );
    ensure!(!notices(c, m, d)?, "仍有未完成的重交要求");
    Ok(())
}
fn audit(c: &Connection, action: &str, subject: &str) -> Result<()> {
    let id = common::id();
    put(
        c,
        "Audit",
        &id,
        &json!({"id":id,"at":common::now(),"action":action,"subject":subject}),
        &Utc::now().format("%Y-%m-%d").to_string(),
    )
}
fn receipt(v: &Value) -> Value {
    json!({"snapshotId":v["id"],"version":v["version"],"receivedAt":v["receivedAt"]})
}

impl Leader {
    pub fn new(root: PathBuf, resource_dir: PathBuf) -> Result<Arc<Self>> {
        let instance = Arc::new(Self {
            store: Store::new(root)?,
            stop: tokio::sync::watch::channel(false).0,
            worker_task: Mutex::new(None),
            job_tasks: Mutex::new(HashMap::new()),
            request_slots: Arc::new(tokio::sync::Semaphore::new(4)),
            self_ref: std::sync::OnceLock::new(),
            resource_dir,
            listener: AsyncMutex::new(None),
            service_error: Mutex::new(None),
            certificate: Mutex::new(None),
            pair_limits: Mutex::new(HashMap::new()),
        });
        let _ = instance.self_ref.set(Arc::downgrade(&instance));
        Ok(instance)
    }
    fn settings(&self) -> Result<Value> {
        self.store.read(|c| {
            get(c, "DepartmentSettings", "settings")?.ok_or_else(|| anyhow!("请先初始化部门"))
        })
    }
    pub async fn start(self: Arc<Self>) -> Result<()> {
        self.store.write(|c| {
            for mut j in all(c, "Job", None)? {
                if s(&j, "state") == "running" {
                    j["state"] = json!("queued");
                    j["error"] = Value::Null;
                    put(c, "Job", s(&j, "id"), &j, s(&j, "day"))?;
                }
            }
            Ok(())
        })?;
        if self.settings().is_ok() {
            if let Err(e) = self.restart_listener().await {
                *self.service_error.lock().unwrap() = Some(e.to_string());
            }
        }
        *self.worker_task.lock().unwrap() = Some(tokio::spawn(self.clone().worker()));
        Ok(())
    }
    pub async fn shutdown(&self) {
        self.stop.send_replace(true);
        let listener = self.listener.lock().await.take();
        if let Some(listener) = listener {
            let _ = listener.await;
        }
        let worker = self.worker_task.lock().unwrap().take();
        if let Some(worker) = worker {
            worker.abort();
            let _ = worker.await;
        }
        let tasks = self.job_tasks.lock().unwrap().drain().map(|(_, task)| task).collect::<Vec<_>>();
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
        let _ = self.store.write(|c| {
            for mut job in all(c, "Job", None)?
                .into_iter()
                .filter(|j| s(j, "state") == "running")
            {
                job["state"] = json!("queued");
                job["error"] = Value::Null;
                put(c, "Job", s(&job, "id"), &job, s(&job, "day"))?;
            }
            Ok(())
        });
    }
    pub async fn dispatch(&self, op: &str, args: Value) -> Result<Value> {
        match op {
            "leader.init" => {
                let (networks, mut network_error) = match self.network_info() {
                    Ok(info) => (info["addresses"].clone(), None),
                    Err(e) => (json!([]), Some(e.to_string())),
                };
                let settings = if self.settings().is_ok() {
                    match self.refresh_address() {
                        Ok(settings) => Some(settings),
                        Err(e) => { network_error = Some(e.to_string()); self.settings().ok() }
                    }
                } else { None };
                Ok(json!({"settings":settings,"networkAddresses":networks,"networkError":network_error,
                    "fingerprint":self.tls_identity()?.1,"serviceError":*self.service_error.lock().unwrap()}))
            }
            "leader.networkAddresses" => self.network_info(),
            "leader.configure" => {
                self.configure(args["settings"].clone(), s(&args, "apiKey"))?;
                self.restart_listener().await?;
                Ok(Value::Null)
            }
            "leader.testConnection" => {
                self.agent(
                    &common::id(),
                    "test",
                    "请回复：连接成功。",
                    json!("测试模型连接"),
                )
                .await?;
                Ok(json!({"message":"连接成功"}))
            }
            "leader.addMember" => self.add_member(s(&args, "name")),
            "leader.invite" => self.invite(s(&args, "memberId")),
            "leader.dashboard" => self.dashboard(s(&args, "day")),
            "leader.history" => self.store.read(|c| {
                day_check(s(&args, "day"))?;
                let mut r = all(c, "Report", Some(s(&args, "day")))?
                    .into_iter()
                    .filter(|v| s(v, "memberId") == s(&args, "memberId"))
                    .collect::<Vec<_>>();
                r.sort_by_key(|v| -n(v, "version"));
                Ok(json!(r))
            }),
            "leader.summaries" => self.store.read(|c| {
                day_check(s(&args, "day"))?;
                let mut a = all(c, "DepartmentReport", Some(s(&args, "day")))?;
                a.sort_by_key(|v| -n(v, "version"));
                Ok(json!(a))
            }),
            "leader.summary" => self.queue("department", "", s(&args, "day")),
            "leader.retry" => {
                self.retry(s(&args, "jobId"))?;
                Ok(Value::Null)
            }
            "leader.cancel" => self.cancel_job(s(&args, "jobId")).await,
            "leader.prepareExport" => self.prepare(s(&args, "reportId")),
            "leader.commitExport" => {
                self.commit(&args["report"])?;
                Ok(Value::Null)
            }
            "leader.getSummary" => self.store.read(|c| {
                let r = get(c, "DepartmentReport", s(&args, "reportId"))?
                    .ok_or_else(|| anyhow!("部门汇总不存在"))?;
                ensure!(
                    !self.summary_stale(c, &r)?,
                    "来源日报或补充已更新，请重新生成部门汇总"
                );
                Ok(r)
            }),
            "leader.backup" => {
                let path = if s(&args, "path").is_empty() {
                    self.store.root.join("backups")
                } else {
                    PathBuf::from(s(&args, "path"))
                };
                Ok(json!({"path":self.backup(&path)?}))
            }
            "leader.restore" => Ok(
                json!({"path":Self::restore(Path::new(s(&args,"path")),Path::new(s(&args,"target")))?}),
            ),
            "leader.revoke" | "leader.generate" | "leader.release" | "leader.reopen"
            | "leader.resubmit" => {
                let ids = args["memberIds"].as_array().cloned().unwrap_or_default();
                let ids = if ids.is_empty() {
                    self.store.read(|c| {
                        Ok(all(c, "Member", None)?
                            .into_iter()
                            .filter(|v| flag(v, "active"))
                            .map(|v| v["id"].clone())
                            .collect::<Vec<_>>())
                    })?
                } else {
                    ids
                };
                let mut errors = Vec::new();
                for id in ids {
                    let mid = id.as_str().unwrap_or("");
                    let result = match op {
                        "leader.generate" => {
                            self.queue("personal", mid, s(&args, "day")).map(|_| ())
                        }
                        _ => self.action(op, mid, s(&args, "day"), s(&args, "reason")),
                    };
                    if let Err(e) = result {
                        errors.push(format!("{mid}：{e}"));
                    }
                }
                if op == "leader.revoke" {
                    ensure!(errors.is_empty(), "{}", errors.join("\n"));
                    Ok(Value::Null)
                } else {
                    Ok(json!({"errors":errors}))
                }
            }
            _ => bail!("未知组长操作：{op}"),
        }
    }
    fn configure(&self, mut settings: Value, key: &str) -> Result<()> {
        ensure!(
            !s(&settings, "name").trim().is_empty()
                && s(&settings, "name").chars().count() <= 100
                && (1024..=65535).contains(&n(&settings, "port")),
            "部门设置无效"
        );
        common::today(s(&settings, "timeZone"))?;
        for (field, default) in [
            ("contextTokens", 65_536_u64),
            ("outputReserveTokens", 8_192),
            ("chunkTokens", 48_000),
        ] {
            if settings[field].is_null() {
                settings[field] = json!(default);
            }
            ensure!(settings[field].as_u64().is_some_and(|v| v > 0), "模型容量设置必须为正整数：{field}");
        }
        ensure!(settings["contextTokens"].as_u64() > settings["outputReserveTokens"].as_u64(), "上下文容量必须大于输出预留");
        if network::automatic(&settings) { network::resolve(&mut settings, &network::addresses()?)?; }
        let url = url::Url::parse(s(&settings, "advertisedUrl"))?;
        ensure!(
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none(),
            "部门地址必须为 HTTPS 根地址"
        );
        if !s(&settings, "apiBaseUrl").is_empty() {
            let api = url::Url::parse(s(&settings, "apiBaseUrl"))?;
            let loopback = matches!(
                api.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
            );
            ensure!(
                (api.scheme() == "https" || api.scheme() == "http" && loopback)
                    && api.username().is_empty()
                    && api.password().is_none()
                    && api.query().is_none(),
                "模型地址需使用 HTTPS（本机测试可使用 HTTP）"
            );
        }
        self.store.write(|c| {
            if let Some(old) = get(c, "DepartmentSettings", "settings")? {
                if s(&settings, "id").is_empty() {
                    settings["id"] = old["id"].clone();
                }
                ensure!(settings["id"] == old["id"], "不能更换部门身份");
                ensure!(
                    settings["timeZone"] == old["timeZone"] || all(c, "Snapshot", None)?.is_empty(),
                    "已有记录后不能修改部门时区"
                );
            } else if s(&settings, "id").is_empty() {
                settings["id"] = json!(common::id());
            }
            if !key.is_empty() {
                common::save_secret(&self.store.root.join("api-key.secret"), key)?;
            }
            put(c, "DepartmentSettings", "settings", &settings, "")?;
            audit(c, "配置部门", s(&settings, "name"))
        })
    }
    fn add_member(&self, name: &str) -> Result<Value> {
        let name = name.trim();
        ensure!(
            (1..=60).contains(&name.chars().count()),
            "姓名长度需为 1–60 字"
        );
        self.store.write(|c| {
            ensure!(
                !all(c, "Member", None)?
                    .iter()
                    .any(|v| flag(v, "active") && s(v, "name") == name),
                "成员姓名已存在"
            );
            let id = common::id();
            let m = json!({"id":id,"name":name,"active":true});
            put(c, "Member", &id, &m, "")?;
            audit(c, "新增成员", &id)?;
            Ok(m)
        })
    }
    fn invite(&self, mid: &str) -> Result<Value> {
        let cfg = self.refresh_address()?;
        let code = format!("{}{}", common::id(), common::id());
        let id = common::hash(&code);
        self.store.write(|c|{member(c,mid)?;put(c,"InviteRecord",&id,&json!({"id":id,"memberId":mid,"expiresAt":(Utc::now()+chrono::Duration::minutes(30)).to_rfc3339(),"pairingRequestId":null,"deviceId":null,"protectedToken":null}),"")?;audit(c,"创建配对邀请",mid)})?;
        let details = json!({"url":cfg["advertisedUrl"],"fingerprint":self.tls_identity()?.1,"code":code,"department":cfg["name"],"modelDestination":cfg["apiBaseUrl"],"timeZone":cfg["timeZone"],"protocol":1});
        Ok(json!({"invitation":STANDARD.encode(serde_json::to_vec(&details)?),"details":details}))
    }
    fn pair(&self, req: &Value) -> Result<Value> {
        ensure!(
            valid_id(s(req, "pairingRequestId"))
                && !s(req, "deviceName").trim().is_empty()
                && s(req, "deviceName").chars().count() <= 100
                && !s(req, "code").trim().is_empty(),
            "配对请求无效"
        );
        self.store.write(|c|{let mut inv=get(c,"InviteRecord",&common::hash(s(req,"code")))?.ok_or_else(||anyhow!("配对码无效或已过期"))?;ensure!(timestamp(&inv,"expiresAt")?>Utc::now(),"配对码无效或已过期");let m=member(c,s(&inv,"memberId"))?;let token;if !inv["pairingRequestId"].is_null(){ensure!(inv["pairingRequestId"]==req["pairingRequestId"],"配对码已使用");ensure!(get(c,"Device",s(&inv,"deviceId"))?.is_some_and(|d|!flag(&d,"revoked")),"设备已撤销");token=common::unprotect(s(&inv,"protectedToken"))?;}else{for mut d in all(c,"Device",None)?.into_iter().filter(|d|d["memberId"]==m["id"]){d["revoked"]=json!(true);put(c,"Device",s(&d,"id"),&d,"")?;}token=format!("{}{}",common::id(),common::id()).to_uppercase();let id=common::id();put(c,"Device",&id,&json!({"id":id,"memberId":m["id"],"name":req["deviceName"],"tokenHash":common::hash(&token),"revoked":false}),"")?;inv["pairingRequestId"]=req["pairingRequestId"].clone();inv["deviceId"]=json!(id);inv["protectedToken"]=json!(common::protect(&token)?);put(c,"InviteRecord",s(&inv,"id"),&inv,"")?;audit(c,"成员设备配对",s(&m,"id"))?;}let cfg=get(c,"DepartmentSettings","settings")?.ok_or_else(||anyhow!("请先初始化部门"))?;Ok(json!({"memberId":m["id"],"memberName":m["name"],"departmentId":cfg["id"],"departmentName":cfg["name"],"token":token,"timeZone":cfg["timeZone"],"modelDestination":cfg["apiBaseUrl"]}))})
    }
    fn authenticate(&self, token: &str) -> Result<String> {
        ensure!(token.len() == 64, "设备身份无效，请重新配对");
        self.store.read(|c| {
            let hash = common::hash(token);
            let d = all(c, "Device", None)?
                .into_iter()
                .find(|v| !flag(v, "revoked") && s(v, "tokenHash") == hash)
                .ok_or_else(|| anyhow!("设备身份无效或已撤销，请重新配对"))?;
            member(c, s(&d, "memberId"))?;
            Ok(s(&d, "memberId").to_owned())
        })
    }
    fn submit(&self, mid: &str, bytes: &[u8]) -> Result<Value> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Upload {
            request_id: String,
            hash: String,
            capture: Box<serde_json::value::RawValue>,
        }
        let upload: Upload = serde_json::from_slice(bytes)?;
        ensure!(valid_id(&upload.request_id), "提交格式无效");
        let text = upload.capture.get();
        ensure!(common::hash(text) == upload.hash, "内容校验失败");
        let cap: Value = serde_json::from_str(text)?;
        self.validate_capture(&cap)?;
        self.store.write(|c|{member(c,mid)?;let key=format!("{mid}:{}",upload.request_id);if let Some(op)=get(c,"Operation",&key)?{ensure!(s(&op,"hash")==upload.hash&&s(&op,"kind")=="upload","请求标识已用于其他内容");return Ok(receipt(&get(c,"Snapshot",s(&op,"resultId"))?.ok_or_else(||anyhow!("原始提交记录缺失"))?));}let day=s(&cap,"day");open(c,mid,day)?;let old=latest(c,"Snapshot",mid,day)?;if let Some(old)=&old {ensure!(timestamp(&cap,"capturedAt")?>=timestamp(old,"capturedAt")?,"已有更新的采集记录，请重新采集后提交");}let id=common::id();let filename=format!("{id}.json.gz");let temp=self.store.root.join("submissions").join(format!("{filename}.tmp"));{use std::io::Write;let mut gzip=flate2::write::GzEncoder::new(std::fs::File::create(&temp)?,flate2::Compression::default());gzip.write_all(text.as_bytes())?;gzip.finish()?.sync_all()?;}std::fs::rename(&temp,self.store.root.join("submissions").join(&filename))?;let snap=json!({"id":id,"memberId":mid,"day":day,"version":old.as_ref().map(|v|n(v,"version")).unwrap_or(0)+1,"requestId":upload.request_id,"hash":upload.hash,"fileName":filename,"receivedAt":common::now(),"capturedAt":cap["capturedAt"],"sessionCount":cap["sessionCount"],"warnings":cap["warnings"]});put(c,"Snapshot",&id,&snap,day)?;put(c,"Operation",&key,&json!({"id":key,"memberId":mid,"hash":upload.hash,"resultId":id,"kind":"upload"}),day)?;for mut notice in all(c,"Notice",Some(day))?.into_iter().filter(|v|s(v,"memberId")==mid&&!flag(v,"fulfilled")){if timestamp(&cap,"capturedAt")?>=timestamp(&notice,"createdAt")?{notice["fulfilled"]=json!(true);put(c,"Notice",s(&notice,"id"),&notice,day)?;}}audit(c,"接收记录",&id)?;Ok(receipt(&snap))})
    }
    fn validate_capture(&self, cap: &Value) -> Result<()> {
        let cfg = self.settings()?;
        let day = s(cap, "day");
        day_check(day)?;
        ensure!(
            n(cap, "protocol") == 1
                && cap["timeZone"] == cfg["timeZone"]
                && day <= common::today(s(&cfg, "timeZone"))?.as_str(),
            "协议、日期或部门时区不匹配"
        );
        let events = cap["events"]
            .as_array()
            .ok_or_else(|| anyhow!("缺少记录"))?;
        let warnings = cap["warnings"]
            .as_array()
            .ok_or_else(|| anyhow!("缺少采集说明"))?;
        let captured = timestamp(cap, "capturedAt")?;
        ensure!(
            events.len() <= 100000
                && warnings.len() <= 10000
                && captured <= Utc::now() + chrono::Duration::minutes(5),
            "采集范围无效或过大"
        );
        let zone = zone(s(&cfg, "timeZone"))?;
        let mut ids = HashSet::new();
        let mut sessions = HashSet::new();
        for e in events {
            let t = timestamp(e, "timestamp")?;
            ensure!(
                !s(e, "id").trim().is_empty()
                    && ids.insert(s(e, "id"))
                    && !s(e, "sessionId").trim().is_empty()
                    && e["text"].is_string()
                    && e["source"].is_string()
                    && matches!(
                        s(e, "kind"),
                        "user" | "assistant" | "tool_call" | "tool_result"
                    )
                    && t <= captured
                    && t.with_timezone(&zone).format("%Y-%m-%d").to_string() == day,
                "记录包含无效事件"
            );
            sessions.insert(s(e, "sessionId"));
        }
        ensure!(
            n(cap, "sessionCount") == sessions.len() as i64
                && warnings
                    .iter()
                    .all(|w| w.as_str().is_some_and(|x| x.chars().count() <= 5000)),
            "记录计数或警告格式错误"
        );
        Ok(())
    }
    /// Acceptance hook: run exactly the checks used by the member upload route.
    pub fn validate_capture_value(&self, capture: &Value) -> Result<()> {
        self.validate_capture(capture)
    }
    fn load_capture(&self, snap: &Value) -> Result<Value> {
        load_capture_at(&self.store.root, snap)
    }
    fn action(&self, op: &str, mid: &str, day: &str, reason: &str) -> Result<()> {
        self.store.write(|c|{member(c,mid)?;if op=="leader.revoke"{for mut d in all(c,"Device",None)?.into_iter().filter(|v|s(v,"memberId")==mid){d["revoked"]=json!(true);put(c,"Device",s(&d,"id"),&d,"")?;}return audit(c,"撤销设备",mid);}day_check(day)?;match op {"leader.release"=>{let mut r=latest(c,"Report",mid,day)?.ok_or_else(||anyhow!("报告不存在"))?;current(c,&r)?;if r["releasedAt"].is_null(){r["releasedAt"]=json!(common::now());put(c,"Report",s(&r,"id"),&r,day)?;}audit(c,"下发报告",s(&r,"id"))},"leader.reopen"=>{let key=format!("{mid}:{day}");put(c,"DayState",&key,&json!({"id":key,"memberId":mid,"day":day,"closed":false}),day)?;audit(c,"重新开启日报",&key)},"leader.resubmit"=>{open(c,mid,day)?;ensure!(!reason.trim().is_empty()&&reason.chars().count()<=1000,"请填写 1–1000 字重交原因");let id=common::id();put(c,"Notice",&id,&json!({"id":id,"memberId":mid,"day":day,"reason":reason.trim(),"createdAt":common::now(),"fulfilled":false}),day)?;audit(c,"要求重交",&format!("{mid}:{day}"))},_=>bail!("未知操作")}})
    }
    #[allow(clippy::possible_missing_else)]
    fn queue(&self, kind: &str, mid: &str, day: &str) -> Result<Value> {
        self.store.write(|c|{day_check(day)?;let mut sid=String::new();if kind=="personal" {member(c,mid)?;open(c,mid,day)?;sid=s(&latest(c,"Snapshot",mid,day)?.ok_or_else(||anyhow!("尚无提交记录"))?,"id").to_owned();ensure!(!notices(c,mid,day)?,"成员尚未按要求重新提交");}if let Some(j)=all(c,"Job",Some(day))?.into_iter().find(|v|s(v,"kind")==kind&&s(v,"memberId")==mid&&pending(v)){return Ok(j);}let id=common::id();let j=json!({"id":id,"kind":kind,"day":day,"memberId":mid,"snapshotId":sid,"state":"queued","createdAt":common::now(),"error":null,"progress":null});put(c,"Job",&id,&j,day)?;Ok(j)})
    }
    fn supplement(&self, mid: &str, req: &Value) -> Result<Value> {
        ensure!(
            valid_id(s(req, "requestId"))
                && req["text"].is_string()
                && (flag(req, "noSupplement") || !s(req, "text").trim().is_empty()),
            "请填写补充；没有补充请填写“无补充说明”"
        );
        let day=self.store.write(|c|{let key=format!("{mid}:{}",s(req,"requestId"));let canonical=json!({"requestId":req["requestId"],"reportId":req["reportId"],"text":req["text"],"noSupplement":req["noSupplement"]});let hash=common::hash(&dotnet_json(&canonical)?);if let Some(old)=get(c,"Operation",&key)?{ensure!(s(&old,"kind")=="feedback"&&s(&old,"hash")==hash,"请求标识已用于其他内容");let original=get(c,"Report",s(&old,"resultId"))?.ok_or_else(||anyhow!("报告不存在"))?;return Ok(s(&original,"day").to_owned());}let mut r=get(c,"Report",s(req,"reportId"))?.ok_or_else(||anyhow!("报告不存在"))?;ensure!(s(&r,"memberId")==mid&&!r["releasedAt"].is_null(),"报告不存在");let day=s(&r,"day").to_owned();open(c,mid,&day)?;current(c,&r)?;ensure!(r["finalizedAt"].is_null(),"此版本已定稿，请等待组长重新生成");let m=member(c,mid)?;r["feedback"]=json!({"text":if flag(req,"noSupplement"){"无补充说明"}else{s(req,"text")},"noSupplement":flag(req,"noSupplement"),"submittedAt":common::now(),"author":m["name"]});put(c,"Report",s(&r,"id"),&r,&day)?;put(c,"Operation",&key,&json!({"id":key,"memberId":mid,"hash":hash,"resultId":r["id"],"kind":"feedback"}),&day)?;audit(c,"收到个人补充",s(&r,"id"))?;Ok(day)})?;
        self.status(mid, &day)
    }
    fn prepare(&self, rid: &str) -> Result<Value> {
        self.store.read(|c| {
            let mut r = get(c, "Report", rid)?.ok_or_else(|| anyhow!("报告不存在"))?;
            current(c, &r)?;
            if !r["finalizedAt"].is_null() {
                ensure!(
                    get(
                        c,
                        "DayState",
                        &format!("{}:{}", s(&r, "memberId"), s(&r, "day"))
                    )?
                    .is_some_and(|v| flag(&v, "closed")),
                    "日报已重新开启，请先生成新版再定稿"
                );
                return Ok(r);
            }
            ensure!(!r["releasedAt"].is_null(), "报告须先下发成员");
            ensure!(
                feedback(&r),
                "请等待成员复核并提交补充说明后导出；没有补充请填写“无补充说明”"
            );
            ensure!(
                !all(c, "Job", Some(s(&r, "day")))?
                    .iter()
                    .any(|v| v["memberId"] == r["memberId"]
                        && s(v, "kind") == "personal"
                        && pending(v)),
                "报告正在重新生成，请稍后定稿"
            );
            r["finalizedAt"] = json!(common::now());
            Ok(r)
        })
    }
    fn commit(&self, finalized: &Value) -> Result<()> {
        self.store.write(|c| {
            let mut r =
                get(c, "Report", s(finalized, "id"))?.ok_or_else(|| anyhow!("报告不存在"))?;
            current(c, &r)?;
            let key = format!("{}:{}", s(&r, "memberId"), s(&r, "day"));
            if !r["finalizedAt"].is_null() {
                ensure!(
                    get(c, "DayState", &key)?.is_some_and(|v| flag(&v, "closed")),
                    "日报已重新开启，请先生成新版再定稿"
                );
                return Ok(());
            }
            ensure!(
                !r["releasedAt"].is_null() && feedback(&r),
                "请等待成员复核并提交补充说明后导出"
            );
            timestamp(finalized, "finalizedAt")?;
            r["finalizedAt"] = finalized["finalizedAt"].clone();
            ensure!(r == *finalized, "报告或补充已更新，请重新定稿");
            ensure!(
                !all(c, "Job", Some(s(&r, "day")))?
                    .iter()
                    .any(|v| s(v, "kind") == "personal"
                        && v["memberId"] == r["memberId"]
                        && pending(v)),
                "报告正在重新生成，请稍后定稿"
            );
            put(c, "Report", s(&r, "id"), &r, s(&r, "day"))?;
            put(
                c,
                "DayState",
                &key,
                &json!({"id":key,"memberId":r["memberId"],"day":r["day"],"closed":true}),
                s(&r, "day"),
            )?;
            audit(c, "定稿", s(&r, "id"))
        })
    }
}
fn zone(name: &str) -> Result<chrono_tz::Tz> {
    common::timezone(name)
}
fn load_capture_at(root: &Path, snap: &Value) -> Result<Value> {
    use std::io::Read;
    ensure!(
        valid_id(s(snap, "id")) && s(snap, "fileName") == format!("{}.json.gz", s(snap, "id")),
        "记录存储路径无效"
    );
    let mut text = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(
        root.join("submissions").join(s(snap, "fileName")),
    )?)
    .take(256 * 1024 * 1024)
    .read_to_string(&mut text)?;
    ensure!(common::hash(&text) == s(snap, "hash"), "存储的记录校验失败");
    Ok(serde_json::from_str(&text)?)
}
// System.Text.Json's default encoder is retained for old feedback request and summary hashes.
pub(crate) fn dotnet_json(v: &Value) -> Result<String> {
    fn string(s: &str) -> String {
        let mut out = String::from("\"");
        for ch in s.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{08}' => out.push_str("\\b"),
                '\u{0c}' => out.push_str("\\f"),
                ch if ch.is_ascii()
                    && !ch.is_ascii_control()
                    && !matches!(ch, '"' | '\'' | '<' | '>' | '&' | '+' | '`') =>
                {
                    out.push(ch)
                }
                _ => {
                    for u in ch.encode_utf16(&mut [0; 2]) {
                        out.push_str(&format!("\\u{u:04X}"));
                    }
                }
            }
        }
        out.push('"');
        out
    }
    Ok(match v {
        Value::String(t) => string(t),
        Value::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(dotnet_json)
                .collect::<Result<Vec<_>>>()?
                .join(",")
        ),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| {
                    // DateTimeOffset is emitted by System.Text.Json's date writer,
                    // which leaves the UTC offset plus sign unescaped.
                    let encoded = if matches!(
                        k.as_str(),
                        "capturedAt" | "createdAt" | "releasedAt" | "finalizedAt" | "submittedAt"
                    ) && v
                        .as_str()
                        .is_some_and(|t| DateTime::parse_from_rfc3339(t).is_ok())
                    {
                        serde_json::to_string(v)?
                    } else {
                        dotnet_json(v)?
                    };
                    Ok(format!("{}:{}", string(k), encoded))
                })
                .collect::<Result<Vec<_>>>()?
                .join(",")
        ),
        _ => v.to_string(),
    })
}

/// Collection warnings that describe known, expected limits are prefixed `说明：` and do
/// not mark a submission as an anomaly; real collection gaps keep the warning badge.
fn collection_problem(warnings: &[Value]) -> bool {
    warnings.iter().any(|warning| {
        let text = warning.as_str().unwrap_or("");
        !text.starts_with("仅覆盖本机")
            && !text.starts_with("未发现可用工作记录")
            && !text.starts_with("说明：")
    })
}

impl Leader {
    fn row(&self, c: &Connection, mid: &str, day: &str) -> Result<Value> {
        let m = member(c, mid)?;
        let snap = latest(c, "Snapshot", mid, day)?.unwrap_or(Value::Null);
        let r = latest(c, "Report", mid, day)?.unwrap_or(Value::Null);
        let notice = notices(c, mid, day)?;
        let mut jobs = all(c, "Job", Some(day))?
            .into_iter()
            .filter(|v| s(v, "kind") == "personal" && s(v, "memberId") == mid)
            .collect::<Vec<_>>();
        jobs.sort_by(|a, b| {
            pending(b)
                .cmp(&pending(a))
                .then_with(|| job_time(b).cmp(job_time(a)))
        });
        let j = jobs.first().cloned().unwrap_or(Value::Null);
        let problem = snap["warnings"]
            .as_array()
            .map(|warnings| collection_problem(warnings))
            .unwrap_or(false);
        let submission = if notice {
            "🟡 待重交"
        } else if snap.is_null() {
            "🔴 未收到"
        } else if problem {
            "🟡 已收到／采集异常"
        } else {
            "🟢 已收到"
        };
        let state = if pending(&j) {
            "生成中".to_owned()
        } else if s(&j, "state") == "failed" && (r.is_null() || job_time(&j) > s(&r, "createdAt")) {
            format!("生成失败：{}", s(&j, "error"))
        } else if r.is_null() && s(&j, "state") == "cancelled" {
            "已取消".into()
        } else if r.is_null() {
            "待生成".into()
        } else if r["snapshotId"] != snap["id"] {
            "来源已更新".into()
        } else if !r["finalizedAt"].is_null() {
            if get(c, "DayState", &format!("{mid}:{day}"))?.is_some_and(|v| flag(&v, "closed")) {
                "已定稿"
            } else {
                "已重新开启／待生成新版"
            }
            .into()
        } else if r["releasedAt"].is_null() {
            "待下发".into()
        } else if r["feedback"].is_null() {
            "待反馈".into()
        } else {
            "已反馈".into()
        };
        Ok(
            json!({"memberId":mid,"name":m["name"],"submissionStatus":submission,"reportStatus":state,"capturedAt":snap["capturedAt"].as_str().unwrap_or("—"),"snapshotVersion":n(&snap,"version"),"reportVersion":n(&r,"version"),"feedbackStatus":if r["feedback"].is_null(){"未反馈"}else if flag(&r["feedback"],"noSupplement"){"无补充"}else{"已补充"},"reportId":r["id"],"analysisProgress":brief(&j),"stagePercent":percent(&j),"jobId":j["id"]}),
        )
    }
    fn status(&self, mid: &str, day: &str) -> Result<Value> {
        self.store.read(|c|{day_check(day)?;member(c,mid)?;let row=self.row(c,mid,day)?;let snap=latest(c,"Snapshot",mid,day)?.unwrap_or(Value::Null);let r=all(c,"Report",Some(day))?.into_iter().filter(|v|s(v,"memberId")==mid&&!v["releasedAt"].is_null()).max_by_key(|v|n(v,"version")).unwrap_or(Value::Null);let cfg=get(c,"DepartmentSettings","settings")?.ok_or_else(||anyhow!("请先初始化部门"))?;let waiting=!r.is_null()&&(r["snapshotId"]!=snap["id"]||latest(c,"Report",mid,day)?.is_some_and(|v|v["id"]!=r["id"]));let state=if r.is_null(){"等待组长下发报告"}else if waiting{"记录或正文已更新，等待组长下发新报告"}else if !r["finalizedAt"].is_null(){"报告已归档"}else if feedback(&r){"补充已提交"}else{"请复核报告并填写补充说明"};let notices=all(c,"Notice",None)?.into_iter().filter(|v|s(v,"memberId")==mid&&!flag(v,"fulfilled")).map(|v|json!({"id":v["id"],"day":v["day"],"reason":v["reason"],"createdAt":v["createdAt"],"fulfilled":v["fulfilled"]})).collect::<Vec<_>>();Ok(json!({"day":day,"submissionStatus":row["submissionStatus"],"reportStatus":state,"receipt":if snap.is_null(){Value::Null}else{receipt(&snap)},"report":r,"notices":notices,"departmentName":cfg["name"],"modelDestination":cfg["apiBaseUrl"],"timeZone":cfg["timeZone"],"capturedAt":snap["capturedAt"],"sessionCount":n(&snap,"sessionCount"),"captureWarnings":snap["warnings"],"analysisProgress":row["analysisProgress"]}))})
    }
    fn summary_input(&self, c: &Connection, day: &str) -> Result<Value> {
        day_check(day)?;
        let mut reports = Vec::new();
        let mut missing = Vec::new();
        for m in all(c, "Member", None)?
            .into_iter()
            .filter(|v| flag(v, "active"))
        {
            let mid = s(&m, "id");
            let r = latest(c, "Report", mid, day)?.unwrap_or(Value::Null);
            let snap = latest(c, "Snapshot", mid, day)?.unwrap_or(Value::Null);
            let reopened = !r["finalizedAt"].is_null()
                && !get(c, "DayState", &format!("{mid}:{day}"))?
                    .is_some_and(|v| flag(&v, "closed"));
            if !r.is_null()
                && feedback(&r)
                && r["snapshotId"] == snap["id"]
                && !notices(c, mid, day)?
                && !reopened
            {
                reports.push(r);
            } else {
                missing.push(format!(
                    "{}：{}",
                    s(&m, "name"),
                    s(&self.row(c, mid, day)?, "reportStatus")
                ));
            }
        }
        let mut contents = reports.clone();
        contents.sort_by(|a, b| s(a, "id").cmp(s(b, "id")));
        for r in &mut contents {
            r["finalizedAt"] = Value::Null;
        }
        let mut sorted = missing.clone();
        sorted.sort();
        let source = json!({"reports":contents,"missing":sorted});
        let signature = common::hash(&dotnet_json(&source)?);
        let cfg =
            get(c, "DepartmentSettings", "settings")?.ok_or_else(|| anyhow!("请先初始化部门"))?;
        Ok(
            json!({"department":cfg["name"],"day":day,"reports":reports,"missing":missing,"signature":signature}),
        )
    }
    fn summary_stale(&self, c: &Connection, r: &Value) -> Result<bool> {
        Ok(self.summary_input(c, s(r, "day"))?["signature"] != r["sourceSignature"])
    }
    fn dashboard(&self, day: &str) -> Result<Value> {
        self.store.read(|c| {
            day_check(day)?;
            let rows = all(c, "Member", None)?
                .iter()
                .filter(|v| flag(v, "active"))
                .map(|m| self.row(c, s(m, "id"), day))
                .collect::<Result<Vec<_>>>()?;
            let mut jobs = all(c, "Job", Some(day))?;
            jobs.sort_by(|a, b| {
                pending(b)
                    .cmp(&pending(a))
                    .then_with(|| job_time(b).cmp(job_time(a)))
            });
            for j in &mut jobs {
                let name = if s(j, "kind") == "department" {
                    "本部门".into()
                } else {
                    get(c, "Member", s(j, "memberId"))?
                        .map(|m| s(&m, "name").to_owned())
                        .unwrap_or_else(|| "未知成员".into())
                };
                j["owner"] = json!(name);
                j["progressText"] = json!(brief(j));
                j["stagePercent"] = json!(percent(j));
                j["details"] = json!(j["progress"]["events"]
                    .as_array()
                    .map(|a| a
                        .iter()
                        .map(|v| format!("{} {}", s(v, "at"), s(v, "message")))
                        .collect::<Vec<_>>()
                        .join("\n"))
                    .unwrap_or_default());
            }
            let summary = all(c, "DepartmentReport", Some(day))?
                .into_iter()
                .max_by_key(|r| n(r, "version"))
                .unwrap_or(Value::Null);
            let stale = !summary.is_null() && self.summary_stale(c, &summary)?;
            Ok(json!({"rows":rows,"jobs":jobs,"summary":summary,"summaryStale":stale}))
        })
    }
    fn retry(&self, id: &str) -> Result<Value> {
        let job = self
            .store
            .read(|c| get(c, "Job", id)?.ok_or_else(|| anyhow!("任务不存在")))?;
        ensure!(s(&job, "state") == "failed", "只能重试失败的任务");
        let day = s(&job, "day");
        let mid = s(&job, "memberId");
        if s(&job, "kind") == "personal" {
            let outdated = self.store.read(|c| {
                member(c, mid)?;
                open(c, mid, day)?;
                ensure!(!notices(c, mid, day)?, "成员尚未按要求重新提交");
                Ok(latest(c, "Snapshot", mid, day)?.is_none_or(|v| v["id"] != job["snapshotId"]))
            })?;
            if outdated {
                return self.queue("personal", mid, day);
            }
        } else {
            let p = self
                .store
                .root
                .join("agent")
                .join(id)
                .join("department-input.json");
            if p.exists() {
                let old: Value = serde_json::from_slice(&std::fs::read(p)?)?;
                if self
                    .store
                    .read(|c| Ok(self.summary_input(c, day)?["signature"] != old["signature"]))?
                {
                    return self.queue("department", "", day);
                }
            }
        }
        self.store.write(|c| {
            if let Some(old) = all(c, "Job", Some(day))?.into_iter().find(|v| {
                v["kind"] == job["kind"] && v["memberId"] == job["memberId"] && pending(v)
            }) {
                return Ok(old);
            }
            let mut resumed = job.clone();
            resumed["state"] = json!("queued");
            resumed["error"] = Value::Null;
            put(c, "Job", id, &resumed, day)?;
            Ok(resumed)
        })
    }
}
fn job_time(j: &Value) -> &str {
    j["progress"]["updatedAt"]
        .as_str()
        .unwrap_or_else(|| s(j, "createdAt"))
}
fn percent(j: &Value) -> f64 {
    let p = &j["progress"];
    if n(p, "total") > 0 {
        (100.0 * n(p, "completed") as f64 / n(p, "total") as f64).clamp(0., 100.)
    } else {
        0.
    }
}
fn brief(j: &Value) -> String {
    match s(j, "state") {
        "queued" => "等待启动或继续 Agent 任务".into(),
        "completed" => "生成完成".into(),
        "failed" => format!("生成失败：{}", s(j, "error")),
        "cancelled" => "已取消生成".into(),
        "running" => format!(
            "{}\n{}",
            s(&j["progress"], "stage"),
            s(&j["progress"], "activity")
        ),
        _ => "—".into(),
    }
}

#[cfg(test)]
mod warning_tests {
    use super::collection_problem;
    use serde_json::json;

    #[test]
    fn informational_warnings_do_not_flag_an_anomaly() {
        assert!(!collection_problem(&[]));
        assert!(!collection_problem(&[
            json!("仅覆盖本机可读取的当日事件；跨日上下文、附件及外部文件未采集。"),
            json!("说明：Trae 导出：1 个导出文件缺少逐条时间戳，已按文件日期计入"),
            json!("未发现可用工作记录；不代表当天没有工作"),
        ]));
    }

    #[test]
    fn real_collection_gaps_still_flag_an_anomaly() {
        assert!(collection_problem(&[json!(
            "DeepSeek Harness：跳过旧版会话格式 v3（sessions/--demo--/session-x/session.v3.jsonl.zstd），仅采集 v4"
        )]));
        assert!(collection_problem(&[json!("Trae 导出：1 个导出文件未识别到对话内容")]));
    }
}