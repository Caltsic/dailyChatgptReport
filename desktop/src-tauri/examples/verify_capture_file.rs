// Validate a capture produced on another platform with the real Leader upload checks.
// Used for the Linux member acceptance: the capture is collected on Ubuntu, then this
// Windows leader build validates the same schema/limits the upload route enforces.
use anyhow::{Context, Result};
use daily_report::{common, leader::Leader};
use serde_json::{json, Value};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|value| value == name)
            .and_then(|index| args.get(index + 1).cloned())
    };
    let capture_path = PathBuf::from(arg("--capture").context("--capture 必填")?);
    let zone = arg("--timezone").unwrap_or_else(|| "Asia/Shanghai".to_owned());
    let capture: Value = serde_json::from_str(&std::fs::read_to_string(&capture_path)?)
        .context("采集文件不是有效 JSON")?;
    let root = std::env::temp_dir().join(format!("capture-check-{}", common::id()));
    std::fs::create_dir_all(&root)?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let leader = Leader::new(root.join("Leader"), root.clone())?;
    leader
        .dispatch(
            "leader.configure",
            json!({"settings":{"id":common::id(),"name":"跨端协议验证部门","timeZone":zone,
                "port":port,"addressMode":"manual","advertisedUrl":format!("https://127.0.0.1:{port}"),
                "apiBaseUrl":"","model":""},"apiKey":""}),
        )
        .await?;
    let result = match leader.validate_capture_value(&capture) {
        Ok(()) => json!({"ok":true,"checkedBy":"leader.validate_capture","day":capture["day"],
            "timeZone":capture["timeZone"],"sessionCount":capture["sessionCount"],
            "events":capture["events"].as_array().map_or(0, Vec::len),
            "sources":capture["sources"]}),
        Err(error) => json!({"ok":false,"error":format!("{error:#}")}),
    };
    leader.shutdown().await;
    let text = serde_json::to_string_pretty(&result)?;
    println!("{text}");
    if let Some(path) = arg("--out") {
        common::atomic_text(&PathBuf::from(path), &text)?;
    }
    anyhow::ensure!(result["ok"] == true, "capture 未通过组长端上传校验");
    Ok(())
}
