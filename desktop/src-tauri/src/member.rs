use crate::{
    collector,
    common::{atomic_text, hash, id, now, read_secret, redact, save_secret, Result},
};
use anyhow::{anyhow, bail, Context};
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::{Client, Method, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fmt, fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpStream, sync::Mutex, time::timeout};
#[path = "member_capture.rs"]
mod capture;

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn identity_part(value: &Value, key: &str) -> String {
    text(&value["identity"], key).to_owned()
}

const FEEDBACK_CONFLICT: &str = "报告正文或工作记录已更新，刚才的补充未提交，已保留在本机。请刷新查看；若新版尚未生成，请组长生成后再复核提交。";

fn feedback_blocked_reason(status: &Value) -> &str {
    let report = &status["report"];
    if report.is_null() {
        return "";
    }
    if report["snapshotId"] != status["receipt"]["snapshotId"] {
        "工作记录已更新，当前仍是旧日报。请组长生成新版；补充可先保存为草稿，待新版到达后复核提交。"
    } else if text(status, "reportStatus") == "记录或正文已更新，等待组长下发新报告" {
        "正文已更新，等待组长下发新版；当前补充保留为草稿，复核新版后再提交。"
    } else {
        ""
    }
}

#[derive(Debug)]
struct ApiError {
    status: u16,
    message: String,
}
impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HTTP {}：{}", self.status, self.message)
    }
}
impl std::error::Error for ApiError {}
impl ApiError {
    fn retryable(&self) -> bool {
        matches!(self.status, 408 | 429) || self.status >= 500
    }
}

struct DepartmentConnection {
    http: Client,
    base: String,
    token: String,
}
impl DepartmentConnection {
    async fn new(pairing: &Value) -> Result<Self> {
        let url = Url::parse(text(pairing, "url")).context("部门地址无效")?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("部门地址必须是 HTTPS 根地址");
        }
        let pin = text(pairing, "fingerprint").replace(':', "").to_uppercase();
        if pin.len() != 64 || !pin.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("部门证书指纹无效");
        }
        let host = url.host_str().context("部门地址缺少主机名")?;
        let port = url.port_or_known_default().unwrap_or(443);

        // This first handshake sends no HTTP data or device token; it only obtains the pinned certificate.
        let connector = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()?;
        let connector = tokio_native_tls::TlsConnector::from(connector);
        let certificate = timeout(Duration::from_secs(5), async {
            let stream = TcpStream::connect((host, port)).await?;
            let stream = connector.connect(host, stream).await?;
            let certificate = stream
                .get_ref()
                .peer_certificate()?
                .context("部门端没有提供证书")?;
            Ok::<_, anyhow::Error>(certificate.to_der()?)
        })
        .await
        .context("连接部门端超时")??;
        let actual = format!("{:X}", Sha256::digest(&certificate));
        if actual != pin {
            bail!("部门证书指纹不匹配，请核对组长提供的配对邀请");
        }
        // Subsequent authenticated requests trust only this certificate, not the machine's CA store.
        let http = Client::builder()
            .use_native_tls()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .tls_built_in_root_certs(false)
            .add_root_certificate(reqwest::Certificate::from_der(&certificate)?)
            .danger_accept_invalid_hostnames(true)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self {
            http,
            base: url.as_str().trim_end_matches('/').to_owned(),
            token: identity_part(pairing, "token"),
        })
    }

    async fn request(&self, method: Method, route: &str, payload: Option<&str>) -> Result<Vec<u8>> {
        let mut request = self.http.request(method, format!("{}{}", self.base, route));
        if !self.token.is_empty() {
            request = request.bearer_auth(&self.token);
        }
        if let Some(payload) = payload {
            request = request
                .header("Content-Type", "application/json")
                .body(payload.to_owned());
        }
        let response = request.send().await?;
        let status = response.status();
        let body = response.bytes().await?;
        if !status.is_success() {
            let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let error = text(&parsed, "error");
            return Err(ApiError {
                status: status.as_u16(),
                message: if error.is_empty() {
                    format!("请求失败（HTTP {}）", status.as_u16())
                } else {
                    redact(error)
                },
            }
            .into());
        }
        Ok(body.to_vec())
    }

    async fn json(&self, method: Method, route: &str, payload: Option<&str>) -> Result<Value> {
        serde_json::from_slice(&self.request(method, route, payload).await?)
            .context("部门端返回的数据格式无效")
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutboxItem {
    id: String,
    created_at: String,
    kind: String,
    payload: String,
    error: Option<String>,
}

/// Tools whose local records can be collected, in the order shown to members.
const SOURCE_TOOLS: [(&str, &str); 2] = [
    ("codex", collector::LABEL_CODEX),
    ("dsh", collector::LABEL_DSH),
];

#[derive(Clone, Deserialize, Serialize)]
struct SourceConfig {
    tool: String,
    enabled: bool,
    #[serde(default)]
    path: String,
}

#[derive(Deserialize, Serialize)]
struct SourcesConfig {
    sources: Vec<SourceConfig>,
}

fn source_index(tool: &str) -> usize {
    SOURCE_TOOLS
        .iter()
        .position(|(name, _)| *name == tool)
        .unwrap_or(usize::MAX)
}

fn source_label(tool: &str) -> &'static str {
    SOURCE_TOOLS
        .iter()
        .find(|(name, _)| *name == tool)
        .map(|(_, label)| *label)
        .unwrap_or("")
}

pub struct MemberClient {
    root: PathBuf,
    sync: Mutex<()>,
    capture: Mutex<Option<capture::CaptureDraft>>,
}

impl MemberClient {
    pub fn new(root: PathBuf) -> Result<Arc<Self>> {
        fs::create_dir_all(root.join("outbox"))?;
        Ok(Arc::new(Self {
            root,
            sync: Mutex::new(()),
            capture: Mutex::new(None),
        }))
    }

    pub fn start(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(10));
            loop {
                timer.tick().await;
                let _ = self.flush().await;
            }
        });
    }

    fn pairing(&self) -> Result<Option<Value>> {
        let path = self.root.join("pairing.secret");
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(
            serde_json::from_str(&read_secret(&path)?).context("本机配对信息无法读取")?,
        ))
    }

    fn require_pairing(&self) -> Result<Value> {
        self.pairing()?.context("请先绑定部门")
    }
    fn public_pairing(pairing: Option<Value>) -> Value {
        match pairing {
            None => Value::Null,
            Some(mut pairing) => {
                if let Some(identity) = pairing["identity"].as_object_mut() {
                    identity.remove("token");
                }
                pairing
            }
        }
    }

    fn default_source(&self, tool: &str) -> SourceConfig {
        match tool {
            "codex" => {
                let legacy = fs::read_to_string(self.root.join("codex-root.txt"))
                    .ok()
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty());
                let path = legacy.unwrap_or_default();
                let effective = if path.is_empty() {
                    collector::default_root()
                } else {
                    PathBuf::from(&path)
                };
                SourceConfig {
                    tool: tool.to_owned(),
                    enabled: effective.is_dir(),
                    path,
                }
            }
            _ => {
                let effective = collector::default_dsh_root();
                SourceConfig {
                    tool: tool.to_owned(),
                    enabled: effective.is_dir(),
                    path: String::new(),
                }
            }
        }
    }

    fn effective_source_path(config: &SourceConfig) -> PathBuf {
        if config.path.trim().is_empty() {
            match config.tool.as_str() {
                "codex" => collector::default_root(),
                _ => collector::default_dsh_root(),
            }
        } else {
            PathBuf::from(config.path.trim())
        }
    }

    fn sources_config(&self) -> Result<Vec<SourceConfig>> {
        let path = self.root.join("sources.json");
        if path.exists() {
            let stored: SourcesConfig =
                serde_json::from_str(&fs::read_to_string(&path)?).context("记录来源配置无法读取")?;
            let mut configs = stored.sources;
            for (tool, _) in SOURCE_TOOLS {
                if !configs.iter().any(|config| config.tool == tool) {
                    configs.push(self.default_source(tool));
                }
            }
            configs.retain(|config| {
                SOURCE_TOOLS
                    .iter()
                    .any(|(tool, _)| *tool == config.tool)
            });
            configs.sort_by_key(|config| source_index(&config.tool));
            return Ok(configs);
        }
        Ok(SOURCE_TOOLS
            .iter()
            .map(|(tool, _)| self.default_source(tool))
            .collect())
    }

    fn save_sources(&self, sources: &[SourceConfig]) -> Result<()> {
        atomic_text(
            &self.root.join("sources.json"),
            &serde_json::to_string(&SourcesConfig {
                sources: sources.to_vec(),
            })?,
        )
    }

    fn source_roots(&self) -> Result<collector::SourceRoots> {
        let mut roots = collector::SourceRoots::default();
        for config in self.sources_config()? {
            if !config.enabled {
                continue;
            }
            let path = Self::effective_source_path(&config);
            match config.tool.as_str() {
                "codex" => roots.codex = Some(path),
                "dsh" => roots.dsh = Some(path),
                _ => {}
            }
        }
        Ok(roots)
    }

    fn source_status(&self) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        for config in self.sources_config()? {
            let effective = Self::effective_source_path(&config);
            rows.push(json!({"tool":config.tool,"label":source_label(&config.tool),
                "enabled":config.enabled,"path":config.path,
                "effectivePath":effective.to_string_lossy(),"detected":effective.is_dir()}));
        }
        Ok(rows)
    }

    fn store_sources(&self, entries: &[Value]) -> Result<Vec<SourceConfig>> {
        let current = self.sources_config()?;
        let mut next = Vec::new();
        for entry in entries {
            let tool = text(entry, "tool");
            if !SOURCE_TOOLS.iter().any(|(name, _)| *name == tool) {
                bail!("未知记录来源：{tool}");
            }
            let enabled = entry["enabled"].as_bool().unwrap_or(false);
            let raw = text(entry, "path").trim().to_owned();
            let path = if raw.is_empty() {
                String::new()
            } else {
                let candidate = PathBuf::from(&raw);
                if !candidate.is_dir() {
                    bail!("记录目录不存在：{raw}");
                }
                candidate.canonicalize()?.to_string_lossy().into_owned()
            };
            if enabled {
                let effective = Self::effective_source_path(&SourceConfig {
                    tool: tool.to_owned(),
                    enabled,
                    path: path.clone(),
                });
                if !effective.is_dir() {
                    bail!("记录目录不存在：{}", effective.display());
                }
            }
            next.push(SourceConfig {
                tool: tool.to_owned(),
                enabled,
                path,
            });
        }
        for (tool, _) in SOURCE_TOOLS {
            if !next.iter().any(|config| config.tool == tool) {
                next.push(
                    current
                        .iter()
                        .find(|config| config.tool == tool)
                        .cloned()
                        .unwrap_or_else(|| self.default_source(tool)),
                );
            }
        }
        next.sort_by_key(|config| source_index(&config.tool));
        self.save_sources(&next)?;
        Ok(next)
    }

    /// Day-selection memory keeps using the Codex path so existing choices survive
    /// the upgrade to multiple sources.
    fn codex_root(&self) -> PathBuf {
        self.sources_config()
            .ok()
            .and_then(|configs| configs.into_iter().find(|config| config.tool == "codex"))
            .map(|config| Self::effective_source_path(&config))
            .unwrap_or_else(collector::default_root)
    }

    fn items(&self) -> Result<Vec<OutboxItem>> {
        let mut items = Vec::new();
        for entry in fs::read_dir(self.root.join("outbox"))? {
            let path = entry?.path();
            if path.extension().and_then(|x| x.to_str()) == Some("secret") {
                match read_secret(&path) {
                    Ok(content) => {
                        items.push(serde_json::from_str(&content).context("待发送记录格式损坏")?)
                    }
                    Err(_) if !path.exists() => continue, // A concurrent flush completed this item.
                    Err(error) => return Err(error),
                }
            }
        }
        items.sort_by(|a: &OutboxItem, b| a.created_at.cmp(&b.created_at));
        Ok(items)
    }

    fn queue(&self) -> Result<Vec<Value>> {
        Ok(self.items()?.into_iter().map(|item| json!({"id":item.id,"createdAt":item.created_at,"kind":item.kind,"error":item.error})).collect())
    }

    fn save_item(&self, item: &OutboxItem) -> Result<()> {
        save_secret(
            &self.root.join("outbox").join(format!("{}.secret", item.id)),
            &serde_json::to_string(item)?,
        )
    }

    fn enqueue(&self, kind: &str, payload: Value) -> Result<OutboxItem> {
        let item = OutboxItem {
            id: id(),
            created_at: now(),
            kind: kind.to_owned(),
            payload: serde_json::to_string(&payload)?,
            error: None,
        };
        self.save_item(&item)?;
        Ok(item)
    }

    fn retain_conflicted_feedback(&self, item: &OutboxItem) -> Result<()> {
        let payload: Value = serde_json::from_str(&item.payload)?;
        let draft = self.draft_path(text(&payload, "reportId"))?;
        if !draft.exists() {
            save_secret(&draft, text(&payload, "text"))?;
        }
        // Keep the exact rejected submission as well as any newer editable draft.
        let archive = self.root.join("feedback-conflicts");
        fs::create_dir_all(&archive)?;
        fs::rename(
            self.root.join("outbox").join(format!("{}.secret", item.id)),
            archive.join(format!("{}.secret", item.id)),
        )?;
        Ok(())
    }

    async fn flush(&self) -> Result<String> {
        let Ok(_guard) = self.sync.try_lock() else {
            return Ok("同步正在进行".to_owned());
        };
        let Some(pairing) = self.pairing()? else {
            return Ok("尚未绑定部门".to_owned());
        };
        let items = self.items()?;
        if items.is_empty() {
            return Ok(String::new());
        }
        let connection = match DepartmentConnection::new(&pairing).await {
            Ok(connection) => connection,
            Err(error) => {
                return Ok(format!(
                    "暂未送达，已保存在本机；{}",
                    redact(&error.to_string())
                ))
            }
        };
        let mut feedback_conflict = false;
        for mut item in items {
            if item.error.is_some() {
                if item.kind == "feedback" && item.error.as_ref().is_some_and(|error|
                    error.starts_with("HTTP 409") && error.contains("正文或来源已更新")) {
                    self.retain_conflicted_feedback(&item)?;
                    feedback_conflict = true;
                }
                continue;
            }
            let route = if item.kind == "upload" {
                "/v1/submissions"
            } else {
                "/v1/supplements"
            };
            match connection
                .json(Method::POST, route, Some(&item.payload))
                .await
            {
                Ok(_) => {
                    fs::remove_file(self.root.join("outbox").join(format!("{}.secret", item.id)))?
                }
                Err(error) => {
                    if let Some(api) = error.downcast_ref::<ApiError>() {
                        if item.kind == "feedback" && api.status == 409
                            && api.message.contains("正文或来源已更新") {
                            item.error = Some(api.to_string());
                            self.save_item(&item)?;
                            self.retain_conflicted_feedback(&item)?;
                            feedback_conflict = true;
                            continue;
                        }
                        if !api.retryable() {
                            item.error = Some(api.to_string());
                            self.save_item(&item)?;
                            if api.status == 401 {
                                break;
                            }
                            continue;
                        }
                    }
                    return Ok(format!(
                        "暂未送达，恢复连接后自动重试；{}",
                        redact(&error.to_string())
                    ));
                }
            }
        }
        let remaining = self.items()?;
        if feedback_conflict {
            Ok(FEEDBACK_CONFLICT.to_owned())
        } else if remaining.is_empty() {
            Ok("部门端已确认接收".to_owned())
        } else {
            Ok(format!(
                "有 {} 项未送达{}",
                remaining.len(),
                remaining
                    .iter()
                    .filter_map(|i| i.error.as_ref())
                    .map(|error| format!("；{error}"))
                    .collect::<String>()
            ))
        }
    }

    fn cache_path(&self, day: &str) -> Result<PathBuf> {
        chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").context("日期格式无效")?;
        Ok(self.root.join(format!("status-{day}.secret")))
    }

    fn cached(&self, day: &str) -> Result<Option<Value>> {
        let path = self.cache_path(day)?;
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(&read_secret(&path)?)?))
    }

    fn draft_path(&self, report_id: &str) -> Result<PathBuf> {
        if report_id.is_empty()
            || !report_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            bail!("报告编号无效");
        }
        Ok(self.root.join(format!("draft-{report_id}.secret")))
    }

    fn draft(&self, report: &Value) -> Result<String> {
        let report_id = text(report, "id");
        if report_id.is_empty() {
            return Ok(String::new());
        }
        let path = self.draft_path(report_id)?;
        if path.exists() {
            read_secret(&path)
        } else {
            Ok(text(&report["feedback"], "text").to_owned())
        }
    }

    async fn status(&self, day: &str) -> Result<Value> {
        let cached = self.cached(day)?;
        let pairing = self.pairing()?;
        let mut sync_message = self.flush().await?;
        let mut status = cached.clone();
        let mut online = false;
        let mut draft_from_previous = false;
        if let Some(pairing) = pairing {
            let fetched = async {
                DepartmentConnection::new(&pairing)
                    .await?
                    .json(Method::GET, &format!("/v1/status/{day}"), None)
                    .await
            }
            .await;
            match fetched {
                Ok(fresh) => {
                    if let Some(previous) = &cached {
                        let report = &fresh["report"];
                        let old_report = &previous["report"];
                        if !text(report, "id").is_empty()
                            && !text(old_report, "id").is_empty()
                            && report["id"] != old_report["id"]
                        {
                            let path = self.draft_path(text(report, "id"))?;
                            if !path.exists() {
                                let old_draft = self.draft(old_report)?;
                                if !old_draft.is_empty() {
                                    save_secret(&path, &old_draft)?;
                                    draft_from_previous = true;
                                }
                            }
                        }
                    }
                    save_secret(&self.cache_path(day)?, &serde_json::to_string(&fresh)?)?;
                    status = Some(fresh);
                    online = true;
                }
                Err(error) => {
                    if !sync_message.is_empty() {
                        sync_message.push('；');
                    }
                    sync_message.push_str(&format!(
                        "当前未连接部门端，显示本机缓存：{}",
                        redact(&error.to_string())
                    ));
                }
            }
        }
        let draft = self.draft(
            status
                .as_ref()
                .map(|s| &s["report"])
                .unwrap_or(&Value::Null),
        )?;
        let blocked = status.as_ref().map(feedback_blocked_reason).unwrap_or("");
        Ok(
            json!({"status":status,"queue":self.queue()?,"syncMessage":sync_message,"draft":draft,
            "online":online,"draftFromPrevious":draft_from_previous,"feedbackBlockedReason":blocked}),
        )
    }

    pub async fn dispatch(&self, op: &str, args: Value) -> Result<Value> {
        match op {
            "member.init" => Ok(
                json!({"pairing":Self::public_pairing(self.pairing()?),"codexRoot":self.codex_root(),
                    "sources":self.source_status()?,"queue":self.queue()?}),
            ),
            "member.pair" => {
                let invitation: Value = serde_json::from_slice(
                    &STANDARD
                        .decode(text(&args, "invitation").trim())
                        .context("请粘贴完整配对邀请")?,
                )?;
                if invitation["protocol"].as_u64().unwrap_or(1) != 1 {
                    bail!("邀请版本不兼容");
                }
                let _guard = self.sync.lock().await;
                let code = text(&invitation, "code");
                if code.is_empty() {
                    bail!("配对邀请缺少配对码");
                }
                let request_path = self.root.join(format!("pair-request-{}.txt", hash(code)));
                let request_id = match fs::read_to_string(&request_path) {
                    Ok(value) => value,
                    Err(_) => {
                        let value = id();
                        atomic_text(&request_path, &value)?;
                        value
                    }
                };
                let payload = json!({"code":code,"deviceName":std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".into()),"pairingRequestId":request_id}).to_string();
                let connection = DepartmentConnection::new(&invitation).await?;
                let identity = connection
                    .json(Method::POST, "/v1/pair", Some(&payload))
                    .await?;
                if let Some(old) = self.pairing()? {
                    if old["identity"]["memberId"] != identity["memberId"]
                        || old["identity"]["departmentId"] != identity["departmentId"]
                    {
                        bail!("此端已绑定其他成员，请使用独立数据目录，避免混入旧队列");
                    }
                }
                let pairing = json!({"url":invitation["url"],"fingerprint":invitation["fingerprint"],"identity":identity});
                save_secret(&self.root.join("pairing.secret"), &pairing.to_string())?;
                for mut item in self.items()? {
                    if item
                        .error
                        .as_ref()
                        .is_some_and(|e| e.starts_with("HTTP 401"))
                    {
                        item.error = None;
                        self.save_item(&item)?;
                    }
                }
                Ok(Self::public_pairing(Some(pairing)))
            }
            "member.status" => self.status(text(&args, "day")).await,
            "member.collect" => self.collect_draft(text(&args, "day")).await,
            "member.previewSession" => self.preview_session(&args).await,
            "member.saveSelection" => self.save_selection(&args).await,
            "member.submit" => self.submit_selected(&args).await,
            "member.draft" => {
                let report_id = text(&args, "reportId");
                if !report_id.is_empty() {
                    save_secret(&self.draft_path(report_id)?, text(&args, "text"))?;
                }
                Ok(Value::Null)
            }
            "member.feedback" => {
                self.require_pairing()?;
                let report_id = text(&args, "reportId");
                let none = args["noSupplement"].as_bool().unwrap_or(false)
                    || text(&args, "text").trim() == "无补充说明";
                let supplement = if none {
                    "无补充说明"
                } else {
                    text(&args, "text")
                };
                if supplement.trim().is_empty() {
                    bail!("请填写补充；没有补充请填写“无补充说明”");
                }
                let draft = self.draft_path(report_id)?;
                if !draft.exists() {
                    save_secret(&draft, supplement)?;
                }
                self.enqueue("feedback", json!({"requestId":id(),"reportId":report_id,"text":supplement,"noSupplement":none}))?;
                Ok(json!({"syncMessage":self.flush().await?}))
            }
            "member.sources" => {
                let entries = args["sources"]
                    .as_array()
                    .context("记录来源配置无效")?;
                self.store_sources(entries)?;
                Ok(json!({"sources":self.source_status()?}))
            }
            "member.codexRoot" => {
                let path = Path::new(text(&args, "path"));
                if !path.is_dir() {
                    bail!("记录目录不存在");
                }
                let canonical = path.canonicalize()?.to_string_lossy().into_owned();
                self.store_sources(&[json!({"tool":"codex","enabled":true,"path":canonical})])?;
                Ok(Value::Null)
            }
            "member.download" => {
                let pairing = self.require_pairing()?;
                let report_id = text(&args, "reportId");
                self.draft_path(report_id)?;
                let bytes = DepartmentConnection::new(&pairing)
                    .await?
                    .request(Method::GET, &format!("/v1/reports/{report_id}/pdf"), None)
                    .await?;
                let path = Path::new(text(&args, "path"));
                fs::write(path, bytes)?;
                Ok(json!({"path":path}))
            }
            "member.retry" => {
                let target = text(&args, "id");
                let _guard = self.sync.lock().await;
                let mut item = self
                    .items()?
                    .into_iter()
                    .find(|i| i.id == target)
                    .context("待发送项不存在")?;
                item.error = None;
                self.save_item(&item)?;
                drop(_guard);
                self.flush().await?;
                Ok(Value::Null)
            }
            "member.discard" | "member.removePending" => {
                let target = text(&args, "id");
                let _guard = self.sync.lock().await;
                let item = self
                    .items()?
                    .into_iter()
                    .find(|i| i.id == target)
                    .context("待发送项不存在")?;
                fs::remove_file(self.root.join("outbox").join(format!("{}.secret", item.id)))?;
                Ok(Value::Null)
            }
            _ => Err(anyhow!("未知成员端操作：{op}")),
        }
    }
}
