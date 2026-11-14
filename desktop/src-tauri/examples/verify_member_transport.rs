// Linux transport acceptance for the member end: a real HTTPS client with certificate
// pinning against a protocol-shaped local server. The stub below only implements the v1
// member endpoints checked here (pair/status/submissions/supplements); it is a test double,
// not the packaged Leader. Run from WSL/Ubuntu with the member feature (no `leader`).
#[cfg(unix)]
mod unix_test {
    use daily_report::{common, member::MemberClient};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::{
        collections::HashMap,
        fs,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    const TOKEN: &str = "LINUX-TRANSPORT-TOKEN-0123456789";

    fn arg(name: &str) -> Option<String> {
        let args: Vec<String> = std::env::args().collect();
        args.iter()
            .position(|value| value == name)
            .and_then(|index| args.get(index + 1).cloned())
    }

    struct Stub {
        identity: native_tls::Identity,
        fingerprint: String,
        received: Arc<Mutex<Vec<Value>>>,
        day: String,
    }

    impl Stub {
        fn new(day: &str) -> anyhow::Result<Self> {
            let key = rcgen::KeyPair::generate()?;
            let mut params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()])?;
            params.distinguished_name.push(rcgen::DnType::CommonName, "CodexDailyReport Linux Test");
            let certificate = params.self_signed(&key)?;
            let identity = native_tls::Identity::from_pkcs8(
                certificate.pem().as_bytes(),
                key.serialize_pem().as_bytes(),
            )?;
            let fingerprint = format!("{:X}", Sha256::digest(certificate.der()));
            Ok(Self {
                identity,
                fingerprint,
                received: Arc::new(Mutex::new(Vec::new())),
                day: day.to_owned(),
            })
        }

        async fn serve(self: Arc<Self>) -> anyhow::Result<u16> {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let port = listener.local_addr()?.port();
            let acceptor = native_tls::TlsAcceptor::new(self.identity.clone())?;
            let acceptor = tokio_native_tls::TlsAcceptor::from(acceptor);
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else { break };
                    let acceptor = acceptor.clone();
                    let stub = self.clone();
                    tokio::spawn(async move {
                        if let Ok(mut stream) = acceptor.accept(stream).await {
                            let _ = stub.handle(&mut stream).await;
                        }
                    });
                }
            });
            Ok(port)
        }

        async fn handle(&self, stream: &mut tokio_native_tls::TlsStream<TcpStream>) -> anyhow::Result<()> {
            let mut buffer = Vec::new();
            loop {
                let Some((method, path, authorized, body)) = read_request(stream, &mut buffer).await? else {
                    return Ok(());
                };
                if !authorized {
                    write_response(stream, 401, &json!({"error":"认证失败"})).await?;
                    continue;
                }
                let (status, payload) = self.route(&method, &path, &body);
                write_response(stream, status, &payload).await?;
            }
        }

        fn route(&self, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
            let parsed: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
            match (method, path) {
                ("POST", "/v1/pair") => {
                    if parsed["code"].as_str() != Some("LINUX-PAIR-CODE") {
                        return (403, json!({"error":"配对码无效"}));
                    }
                    (
                        200,
                        json!({"memberId":"m-linux","memberName":"Linux 验证成员",
                            "departmentId":"d-linux","departmentName":"跨端验证部门","token":TOKEN,
                            "timeZone":"Asia/Shanghai","modelDestination":"https://model.example/v1"}),
                    )
                }
                ("GET", route) if route.starts_with("/v1/status/") => (
                    200,
                    json!({"day":self.day,"submissionStatus":"已收到","reportStatus":"等待组长下发报告",
                        "receipt":Value::Null,"report":Value::Null,"notices":[],
                        "departmentName":"跨端验证部门","modelDestination":"https://model.example/v1",
                        "timeZone":"Asia/Shanghai","capturedAt":common::now(),"sessionCount":2,
                        "captureWarnings":[],"analysisProgress":""}),
                ),
                ("POST", "/v1/submissions") => {
                    let capture = &parsed["capture"];
                    if parsed["hash"].as_str() != Some(common::hash(&capture.to_string()).as_str()) {
                        return (400, json!({"error":"内容校验失败"}));
                    }
                    if capture["protocol"] != 1 || capture["timeZone"] != "Asia/Shanghai" {
                        return (400, json!({"error":"协议或时区不匹配"}));
                    }
                    let events = capture["events"].as_array().cloned().unwrap_or_default();
                    let sessions = events
                        .iter()
                        .map(|event| event["sessionId"].as_str().unwrap_or_default().to_owned())
                        .collect::<std::collections::HashSet<_>>();
                    if events.is_empty()
                        || capture["sessionCount"].as_u64() != Some(sessions.len() as u64)
                        || events.iter().any(|event| {
                            !matches!(
                                event["kind"].as_str().unwrap_or_default(),
                                "user" | "assistant" | "tool_call" | "tool_result"
                            ) || event["id"].as_str().unwrap_or_default().is_empty()
                        })
                    {
                        return (400, json!({"error":"记录格式无效"}));
                    }
                    if let Ok(mut received) = self.received.lock() {
                        received.push(capture.clone());
                    }
                    (
                        200,
                        json!({"version":1,"receivedAt":common::now(),"sessionCount":sessions.len(),
                            "eventCount":events.len(),"capturedAt":capture["capturedAt"]}),
                    )
                }
                ("POST", "/v1/supplements") => (200, json!({"receivedAt":common::now()})),
                _ => (404, json!({"error":"未找到"})),
            }
        }

        fn received(&self) -> Vec<Value> {
            self.received.lock().map(|value| value.clone()).unwrap_or_default()
        }
    }

    // The status request carries no JSON body; the route only needs its method and prefix.

    async fn read_request(
        stream: &mut tokio_native_tls::TlsStream<TcpStream>,
        buffer: &mut Vec<u8>,
    ) -> anyhow::Result<Option<(String, String, bool, Vec<u8>)>> {
        loop {
            if let Some(header_end) = find(buffer, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
                let mut lines = head.lines();
                let Some(request) = lines.next() else {
                    return Ok(None);
                };
                let mut parts = request.split_whitespace();
                let method = parts.next().unwrap_or_default().to_owned();
                let path = parts.next().unwrap_or_default().to_owned();
                let headers = lines
                    .filter_map(|line| line.split_once(':'))
                    .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
                    .collect::<HashMap<_, _>>();
                let authorized = path == "/v1/pair"
                    || headers
                        .get("authorization")
                        .is_some_and(|value| value == &format!("Bearer {TOKEN}"));
                let length = headers
                    .get("content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let total = header_end + 4 + length;
                if buffer.len() >= total {
                    let body = buffer[header_end + 4..total].to_vec();
                    buffer.drain(..total);
                    return Ok(Some((method, path, authorized, body)));
                }
            }
            let mut chunk = [0u8; 8192];
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Ok(None);
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
    }

    async fn write_response(
        stream: &mut tokio_native_tls::TlsStream<TcpStream>,
        status: u16,
        payload: &Value,
    ) -> anyhow::Result<()> {
        let body = serde_json::to_string(payload)?;
        let reason = if status == 200 { "OK" } else { "Error" };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await?;
        stream.flush().await?;
        Ok(())
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|window| window == needle)
    }

    fn base64(value: &Value) -> anyhow::Result<String> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        Ok(STANDARD.encode(serde_json::to_vec(value)?))
    }

    fn fixtures(root: &Path) -> anyhow::Result<(PathBuf, PathBuf)> {
        let codex = root.join("codex");
        fs::create_dir_all(codex.join("sessions"))?;
        let lines = [
            json!({"type":"session_meta","payload":{"id":"linux-codex","title":"Linux Codex 会话","cwd":"/workspace/demo"}}),
            json!({"timestamp":common::now(),"type":"response_item","payload":{"type":"message","role":"user",
                "content":[{"type":"input_text","text":"LINUX_CODEX_USER 整理导出"}]}}),
            json!({"timestamp":common::now(),"type":"response_item","payload":{"type":"message","role":"assistant","channel":"final",
                "content":[{"type":"output_text","text":"LINUX_CODEX_ASSISTANT 已完成"}]}}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        fs::write(codex.join("sessions/linux.jsonl"), format!("{lines}\n"))?;

        let dsh = root.join("dsh");
        let directory = dsh.join("sessions/--workspace-demo--/session-linux");
        fs::create_dir_all(&directory)?;
        let now = chrono::Utc::now().timestamp_millis();
        let rows = [
            json!({"type":"session","version":4,"id":"session-linux","createdAt":now-60_000,
                "cwd":"/workspace/demo","isSeeded":false,"delegationDepth":0,"agentPreset":"standard"}),
            json!({"type":"user/message","seq":1,"time":now-9_000,
                "data":{"content":[{"type":"text","text":"LINUX_DSH_USER 排查接口超时"}],
                    "source":{"kind":"user"},"role":"user","id":"m1"}}),
            json!({"type":"user/message","seq":2,"time":now-8_500,
                "data":{"content":[{"type":"text","text":"LINUX_INJECTED_MUST_NOT_APPEAR"}],
                    "source":{"kind":"runtime-context"},"role":"user","id":"m2"}}),
            json!({"type":"system/message","seq":3,"time":now-8_000,
                "data":{"turn":1,"step":1,"message":{"role":"system",
                    "content":[{"type":"text","text":"LINUX_SYSTEM_PROMPT_MUST_NOT_APPEAR"}],
                    "source":{"kind":"system-prompt"},"id":"s1"}}}),
            json!({"type":"assistant/message","seq":4,"time":now-7_000,
                "data":{"turn":1,"step":1,"message":{"role":"assistant","content":[
                    {"type":"reasoning","text":"LINUX_REASONING_MUST_NOT_APPEAR"},
                    {"type":"text","text":"LINUX_DSH_ASSISTANT 已定位下游等待"}]}}}),
            json!({"type":"tool/call","seq":5,"time":now-6_000,
                "data":{"turn":1,"step":1,"callId":"call-1","name":"bash","arguments":"{\"command\":\"grep timeout app.log\"}"}}),
            json!({"type":"tool/result","seq":6,"time":now-5_000,
                "data":{"turn":1,"step":1,"message":{"role":"tool","source":{"kind":"tool","callId":"call-1"},
                    "toolCallId":"call-1","content":[{"type":"text","text":"LINUX_DSH_TOOL_RESULT 命中 2 处"}],"isError":false}}}),
        ];
        let mut bytes = Vec::new();
        for chunk in [&rows[..1], &rows[1..]] {
            let payload = format!(
                "{}\n",
                chunk.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")
            );
            bytes.extend(zstd::encode_all(payload.as_bytes(), 3)?);
        }
        fs::write(directory.join("session.v4.jsonl.zstd"), bytes)?;
        Ok((codex, dsh))
    }

    pub async fn run() -> anyhow::Result<()> {
        let out = PathBuf::from(arg("--out").expect("--out 必填"));
        let root = out.parent().unwrap().join("transport-work");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root)?;
        let day = common::today("Asia/Shanghai")?;
        let stub = Arc::new(Stub::new(&day)?);
        let port = stub.clone().serve().await?;
        let url = format!("https://127.0.0.1:{port}");
        let invitation = base64(&json!({
            "url":url,"fingerprint":stub.fingerprint,"code":"LINUX-PAIR-CODE",
            "department":"跨端验证部门","modelDestination":"https://model.example/v1",
            "timeZone":"Asia/Shanghai","protocol":1
        }))?;
        let (codex, dsh) = fixtures(&root)?;

        let member_root = root.join("member");
        let member = MemberClient::new(member_root.clone())?;
        let paired = member
            .dispatch("member.pair", json!({"invitation":invitation}))
            .await?;
        anyhow::ensure!(paired["identity"]["memberId"] == "m-linux", "配对身份不符：{paired}");

        // A wrong fingerprint must be rejected during the pinned TLS handshake.
        let wrong_member = MemberClient::new(root.join("member-wrong"))?;
        let wrong_invitation = base64(&json!({
            "url":url,"fingerprint":"00".repeat(32),"code":"LINUX-PAIR-CODE",
            "department":"跨端验证部门","modelDestination":"https://model.example/v1",
            "timeZone":"Asia/Shanghai","protocol":1
        }))?;
        let wrong_error = wrong_member
            .dispatch("member.pair", json!({"invitation":wrong_invitation}))
            .await
            .err()
            .map(|error| format!("{error:#}"))
            .unwrap_or_default();
        anyhow::ensure!(wrong_error.contains("指纹不匹配"), "错误指纹未被拒绝：{wrong_error}");

        member
            .dispatch(
                "member.sources",
                json!({"sources":[
                    {"tool":"codex","enabled":true,"path":codex.to_string_lossy()},
                    {"tool":"dsh","enabled":true,"path":dsh.to_string_lossy()}
                ]}),
            )
            .await?;
        let draft = member.dispatch("member.collect", json!({"day":day})).await?;
        let sessions = draft["sessions"].as_array().cloned().unwrap_or_default();
        anyhow::ensure!(sessions.len() == 2, "应采集 2 个来源会话：{sessions:?}");
        let selected = sessions.iter().map(|session| session["id"].clone()).collect::<Vec<_>>();
        let submitted = member
            .dispatch("member.submit", json!({"draftId":draft["draftId"],"sessionIds":selected}))
            .await?;
        anyhow::ensure!(submitted["received"] == true, "部门端未确认接收：{submitted}");
        let status = member.dispatch("member.status", json!({"day":day})).await?;
        anyhow::ensure!(status["online"] == true, "TLS 状态查询未成功：{status}");
        anyhow::ensure!(
            status["queue"].as_array().map(|queue| queue.is_empty()).unwrap_or(false),
            "上传后队列未清空：{status}"
        );

        let received = stub.received();
        anyhow::ensure!(received.len() == 1, "服务端收到 {} 次上传", received.len());
        let capture = &received[0];
        let serialized = capture.to_string();
        for internal in [
            "LINUX_SYSTEM_PROMPT_MUST_NOT_APPEAR",
            "LINUX_REASONING_MUST_NOT_APPEAR",
            "LINUX_INJECTED_MUST_NOT_APPEAR",
        ] {
            anyhow::ensure!(!serialized.contains(internal), "上传内容包含内部记录：{internal}");
        }
        anyhow::ensure!(
            serialized.contains("dsh:session-linux") && serialized.contains("linux-codex"),
            "上传内容缺少两类来源"
        );
        let result = json!({
            "ok":true,"day":day,"url":url,"paired":paired,"sessions":sessions,
            "submitted":submitted,"status":status,"receivedCapture":capture,
            "wrongFingerprintRejected":wrong_error,
            "verified":[
                "Linux native-tls handshake against a real HTTPS listener",
                "certificate pinning accepts the expected fingerprint and rejects a wrong one",
                "pairing stores identity and bearer token",
                "Codex and DeepSeek Harness sessions collected and selected locally",
                "HTTPS POST /v1/submissions accepted after protocol/hash/kind checks",
                "GET /v1/status returns online with an empty queue",
                "DSH internal records absent from the uploaded capture"
            ],
            "modelCalled":false
        });
        common::atomic_text(&out, &serde_json::to_string_pretty(&result)?)?;
        println!("Linux member transport passed: {}", out.display());
        Ok(())
    }
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    unix_test::run().await
}

#[cfg(not(unix))]
fn main() {
    println!("verify_member_transport 仅在 Linux/WSL 上运行");
}
