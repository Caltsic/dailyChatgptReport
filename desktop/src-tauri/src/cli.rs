use crate::{common, member::MemberClient, ROLE};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::fs;

fn value(args: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .or_else(|| {
            args.iter()
                .find_map(|arg| arg.strip_prefix(&prefix).map(str::to_owned))
        })
}

fn values(args: &[String], name: &str) -> Vec<String> {
    let mut result = Vec::new();
    let prefix = format!("{name}=");
    let mut index = 0;
    while index < args.len() {
        if args[index] == name {
            if let Some(next) = args.get(index + 1) {
                result.push(next.clone());
                index += 1;
            }
        } else if let Some(next) = args[index].strip_prefix(&prefix) {
            result.push(next.to_owned());
        }
        index += 1;
    }
    result
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

fn json_output(args: &[String]) -> bool {
    has(args, "--json")
}

fn print_value(command: &str, args: &[String], value: &Value) -> Result<()> {
    if json_output(args) || !matches!(command, "status" | "collect" | "submit" | "supplement") {
        println!("{}", serde_json::to_string_pretty(value)?);
        return Ok(());
    }
    match command {
        "status" => {
            let status = &value["status"];
            let report = status["report"]
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("无");
            println!(
                "在线：{}；记录：{}；日报：{}；队列：{}；{}",
                value["online"].as_bool().unwrap_or(false),
                status["submissionStatus"].as_str().unwrap_or("未知"),
                report,
                value["queue"].as_array().map(Vec::len).unwrap_or(0),
                value["syncMessage"].as_str().unwrap_or("")
            );
        }
        "collect" => {
            let sessions = value["sessions"].as_array().cloned().unwrap_or_default();
            println!(
                "采集完成：{} 个会话；draftId={}",
                sessions.len(),
                value["draftId"].as_str().unwrap_or("")
            );
            for session in sessions {
                println!(
                    "{}\t{}\t选择={}\t事件={}",
                    session["id"].as_str().unwrap_or(""),
                    session["title"].as_str().unwrap_or(""),
                    session["selected"].as_bool().unwrap_or(false),
                    session["eventCount"].as_u64().unwrap_or(0)
                );
            }
        }
        "submit" => println!(
            "提交：{}；会话={}；事件={}；{}",
            if value["received"].as_bool().unwrap_or(false) {
                "部门端已确认接收"
            } else {
                "已保存到本机队列，等待送达"
            },
            value["sessionCount"].as_u64().unwrap_or(0),
            value["eventCount"].as_u64().unwrap_or(0),
            value["syncMessage"].as_str().unwrap_or("")
        ),
        "supplement" => println!(
            "补充：{}",
            value["syncMessage"].as_str().unwrap_or("已保存")
        ),
        _ => println!("{}", serde_json::to_string_pretty(value)?),
    }
    Ok(())
}

fn help() {
    println!(
        r#"Linux 成员端命令行：
  CodexDailyReport.Member cli init [--json]
  CodexDailyReport.Member cli pair --invitation BASE64 | --invitation-file FILE [--json]
  CodexDailyReport.Member cli sources [--json]
  CodexDailyReport.Member cli collect [--day YYYY-MM-DD] [--json]
  CodexDailyReport.Member cli submit [--day YYYY-MM-DD] [--session ID ... | --all | --none] [--json]
  CodexDailyReport.Member cli status [--day YYYY-MM-DD] [--json]
  CodexDailyReport.Member cli supplement [--day YYYY-MM-DD] [--report-id ID] (--text TEXT | --file FILE | --no-supplement) [--json]

全局选项：--data-dir DIR 使用独立成员数据目录；不指定日期时使用绑定部门时区的当天。
submit 会在同一次进程中重新采集、按会话筛选并上传；默认上传本次采集发现的全部会话。
"#
    );
}

async fn day_for(member: &MemberClient, args: &[String]) -> Result<String> {
    if let Some(day) = value(args, "--day") {
        return Ok(day);
    }
    let init = member.dispatch("member.init", json!({})).await?;
    let zone = init["pairing"]["identity"]["timeZone"]
        .as_str()
        .unwrap_or("Asia/Shanghai");
    common::today(zone)
}

fn selected_ids(draft: &Value, args: &[String]) -> Result<Vec<String>> {
    if has(args, "--none") {
        return Ok(Vec::new());
    }
    let explicit = values(args, "--session");
    if !explicit.is_empty() {
        return Ok(explicit);
    }
    let sessions = draft["sessions"]
        .as_array()
        .context("采集结果缺少会话列表")?;
    Ok(sessions
        .iter()
        .filter(|session| session["selected"].as_bool().unwrap_or(false) || has(args, "--all"))
        .filter_map(|session| session["id"].as_str().map(str::to_owned))
        .collect())
}

pub async fn run(args: &[String]) -> Result<()> {
    if ROLE == "Leader" {
        bail!("组长端不提供成员端命令行接口");
    }
    let start = args
        .iter()
        .position(|arg| arg == "cli" || arg == "--cli")
        .map(|index| index + 1)
        .unwrap_or(0);
    let mut command = "help";
    let mut index = start;
    while index < args.len() {
        if args[index] == "--data-dir" {
            index += 2;
            continue;
        }
        if args[index] == "--json" || args[index].starts_with("--data-dir=") {
            index += 1;
            continue;
        }
        if !args[index].starts_with("--") {
            command = args[index].as_str();
            break;
        }
        index += 1;
    }
    if matches!(command, "help" | "--help" | "-h") {
        help();
        return Ok(());
    }
    let member = MemberClient::new(crate::data_root())?;
    match command {
        "init" | "sources" => {
            let result = if command == "sources"
                && ["--codex", "--claude", "--agy", "--dsh", "--zcode", "--trae-local", "--trae"]
                    .iter()
                    .any(|name| value(args, name).is_some())
            {
                let entries = [
                    ("codex", "--codex"),
                    ("claude", "--claude"),
                    ("agy", "--agy"),
                    ("dsh", "--dsh"),
                    ("zcode", "--zcode"),
                    ("traeLocal", "--trae-local"),
                    ("trae", "--trae"),
                ]
                .into_iter()
                .filter_map(|(tool, flag)| {
                    value(args, flag).map(|path| json!({"tool":tool,"enabled":true,"path":path}))
                })
                .collect::<Vec<_>>();
                member
                    .dispatch("member.sources", json!({"sources": entries}))
                    .await?
            } else {
                member.dispatch("member.init", json!({})).await?
            };
            print_value(command, args, &result)
        }
        "pair" => {
            let invitation = if let Some(path) = value(args, "--invitation-file") {
                fs::read_to_string(path)
                    .context("无法读取配对邀请文件")?
                    .trim()
                    .to_owned()
            } else {
                value(args, "--invitation").context("请提供 --invitation 或 --invitation-file")?
            };
            let result = member
                .dispatch("member.pair", json!({"invitation":invitation}))
                .await?;
            print_value(command, args, &result)
        }
        "collect" => {
            let day = day_for(&member, args).await?;
            let result = member
                .dispatch("member.collect", json!({"day":day}))
                .await?;
            print_value(command, args, &result)
        }
        "submit" => {
            let day = day_for(&member, args).await?;
            let draft = member
                .dispatch("member.collect", json!({"day":day}))
                .await?;
            let ids = selected_ids(&draft, args)?;
            let result = member
                .dispatch(
                    "member.submit",
                    json!({"draftId":draft["draftId"],"sessionIds":ids}),
                )
                .await?;
            print_value(command, args, &result)
        }
        "status" => {
            let day = day_for(&member, args).await?;
            let result = member.dispatch("member.status", json!({"day":day})).await?;
            print_value(command, args, &result)
        }
        "supplement" | "confirm" => {
            let day = day_for(&member, args).await?;
            let status = member.dispatch("member.status", json!({"day":day})).await?;
            let report_id = value(args, "--report-id")
                .or_else(|| status["status"]["report"]["id"].as_str().map(str::to_owned))
                .context("当前没有可确认的日报，请先填写 --report-id 或等待组长下发日报")?;
            let no_supplement = has(args, "--no-supplement");
            let text = if no_supplement {
                "无补充说明".to_owned()
            } else if let Some(path) = value(args, "--file") {
                fs::read_to_string(path).context("无法读取补充说明文件")?
            } else {
                value(args, "--text").context("请提供 --text、--file 或 --no-supplement")?
            };
            let result = member
                .dispatch(
                    "member.feedback",
                    json!({"reportId":report_id,"text":text,"noSupplement":no_supplement}),
                )
                .await?;
            print_value("supplement", args, &result)
        }
        _ => bail!("未知命令：{command}；使用 cli help 查看用法"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_session_flags_are_preserved() {
        let args = vec![
            "--session".to_owned(),
            "a".to_owned(),
            "--session=b".to_owned(),
        ];
        assert_eq!(values(&args, "--session"), vec!["a", "b"]);
    }

    #[test]
    fn explicit_selection_overrides_default() {
        let draft = json!({"sessions":[{"id":"a","selected":true},{"id":"b","selected":false}]});
        assert_eq!(selected_ids(&draft, &[]).unwrap(), vec!["a"]);
        assert_eq!(
            selected_ids(&draft, &["--all".to_owned()]).unwrap(),
            vec!["a", "b"]
        );
        assert!(selected_ids(&draft, &["--none".to_owned()])
            .unwrap()
            .is_empty());
    }
}
