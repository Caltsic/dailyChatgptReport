use anyhow::{ensure, Result};
use chrono::DateTime;
use pulldown_cmark::{Event, Options, Parser, Tag};
use serde_json::Value;

// PDF HTML is rendered from these paragraphs, runs and table cells.
// User supplements are never parsed as Markdown, and HTML/images never execute.
#[derive(Clone, Default)]
struct Run {
    text: String,
    bold: bool,
    italic: bool,
    code: bool,
    strike: bool,
    link: bool,
}
struct Paragraph {
    level: u8, // 0 = normal, 1..=3 = heading, 9 = title
    runs: Vec<Run>,
    indent: usize,
    quote: bool,
    code: bool,
}
struct Row {
    header: bool,
    cells: Vec<Vec<Run>>,
}
enum Block {
    Paragraph(Paragraph),
    Table { rows: Vec<Row>, source: bool },
}
#[derive(Clone)]
enum Kind {
    Paragraph,
    Heading(u8),
    Code,
    Html,
    Quote,
    List(Option<u64>),
    Item,
    Table,
    Head,
    Row,
    Cell,
    Bold,
    Italic,
    Strike,
    Link(String),
    Image(String),
    Other,
}
enum Node {
    Text(String),
    InlineCode(String),
    Break,
    Rule,
    Group(Kind, Vec<Node>),
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
fn number(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}
fn xml(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\r' | '\n' => out.push(c),
            c if c >= ' ' && c != '\u{fffe}' && c != '\u{ffff}' => out.push(c),
            _ => {}
        }
    }
    out
}
fn plain(value: impl Into<String>) -> Vec<Run> {
    vec![Run {
        text: value.into(),
        ..Run::default()
    }]
}
fn para(blocks: &mut Vec<Block>, level: u8, text: impl Into<String>) {
    blocks.push(Block::Paragraph(Paragraph {
        level,
        runs: plain(text),
        indent: 0,
        quote: false,
        code: false,
    }));
}
fn timestamp(value: &str, zone: &str) -> String {
    if let Ok(time) = DateTime::parse_from_rfc3339(value) {
        if let Ok(tz) = crate::common::timezone(zone) {
            return time
                .with_timezone(&tz)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();
        }
        return time.format("%Y-%m-%d %H:%M:%S %:z").to_string();
    }
    if value.is_empty() {
        "—".into()
    } else {
        value.into()
    }
}

fn markdown(text: &str) -> Vec<Block> {
    let mut stack: Vec<(Kind, Vec<Node>)> = vec![(Kind::Other, Vec::new())];
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(tag) => {
                let kind = match tag {
                    Tag::Paragraph => Kind::Paragraph,
                    Tag::Heading { level, .. } => Kind::Heading((level as u8).min(3)),
                    Tag::CodeBlock(_) => Kind::Code,
                    Tag::HtmlBlock => Kind::Html,
                    Tag::BlockQuote(_) => Kind::Quote,
                    Tag::List(start) => Kind::List(start),
                    Tag::Item => Kind::Item,
                    Tag::Table(_) => Kind::Table,
                    Tag::TableHead => Kind::Head,
                    Tag::TableRow => Kind::Row,
                    Tag::TableCell => Kind::Cell,
                    Tag::Strong => Kind::Bold,
                    Tag::Emphasis => Kind::Italic,
                    Tag::Strikethrough => Kind::Strike,
                    Tag::Link { dest_url, .. } => Kind::Link(dest_url.to_string()),
                    Tag::Image { dest_url, .. } => Kind::Image(dest_url.to_string()),
                    _ => Kind::Other,
                };
                stack.push((kind, Vec::new()));
            }
            Event::End(_) => {
                if stack.len() > 1 {
                    let (kind, children) = stack.pop().unwrap();
                    stack
                        .last_mut()
                        .unwrap()
                        .1
                        .push(Node::Group(kind, children));
                }
            }
            event => {
                let node = match event {
                    Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => {
                        Node::Text(t.into_string())
                    }
                    Event::Code(t) => Node::InlineCode(t.into_string()),
                    Event::SoftBreak | Event::HardBreak => Node::Break,
                    Event::Rule => Node::Rule,
                    Event::TaskListMarker(done) => {
                        Node::Text(if done { "[已完成] " } else { "[ ] " }.into())
                    }
                    Event::FootnoteReference(t) => Node::Text(format!("[{}]", t)),
                    Event::InlineMath(t) | Event::DisplayMath(t) => Node::Text(t.into_string()),
                    _ => continue,
                };
                stack.last_mut().unwrap().1.push(node);
            }
        }
    }
    let mut out = Vec::new();
    collect(&stack.remove(0).1, 0, false, None, &mut out);
    out
}

fn inline(nodes: &[Node], style: &Run, out: &mut Vec<Run>) {
    for node in nodes {
        let mut run = style.clone();
        match node {
            Node::Text(text) => {
                run.text = text.clone();
                out.push(run);
            }
            Node::InlineCode(text) => {
                run.text = text.clone();
                run.code = true;
                out.push(run);
            }
            Node::Break => {
                run.text = "\n".into();
                out.push(run);
            }
            Node::Rule => {
                run.text = "────────".into();
                out.push(run);
            }
            Node::Group(kind, children) => {
                match kind {
                    Kind::Bold => run.bold = true,
                    Kind::Italic => run.italic = true,
                    Kind::Strike => run.strike = true,
                    Kind::Code => run.code = true,
                    Kind::Link(_) => run.link = true,
                    Kind::Image(_) => {
                        let mut label = run.clone();
                        label.text = "图片：".into();
                        out.push(label);
                    }
                    _ => {}
                }
                inline(children, &run, out);
                if let Kind::Link(url) | Kind::Image(url) = kind {
                    if !url.is_empty() {
                        run.text = format!("（{}）", url);
                        out.push(run);
                    }
                }
            }
        }
    }
}
fn paragraph_from(
    nodes: &[Node],
    level: u8,
    depth: usize,
    quote: bool,
    code: bool,
    prefix: &mut Option<String>,
) -> Block {
    let mut runs = Vec::new();
    if let Some(text) = prefix.take() {
        runs.extend(plain(text));
    }
    inline(
        nodes,
        &Run {
            code,
            ..Run::default()
        },
        &mut runs,
    );
    Block::Paragraph(Paragraph {
        level,
        runs,
        indent: depth,
        quote,
        code,
    })
}
fn collect(
    nodes: &[Node],
    depth: usize,
    quote: bool,
    mut prefix: Option<String>,
    out: &mut Vec<Block>,
) {
    let mut start = 0;
    for (index, node) in nodes.iter().enumerate() {
        let is_block = matches!(
            node,
            Node::Group(
                Kind::Paragraph
                    | Kind::Heading(_)
                    | Kind::Code
                    | Kind::Html
                    | Kind::Quote
                    | Kind::List(_)
                    | Kind::Table,
                _
            ) | Node::Rule
        );
        if !is_block {
            continue;
        }
        if start < index {
            out.push(paragraph_from(
                &nodes[start..index],
                0,
                depth,
                quote,
                false,
                &mut prefix,
            ));
        }
        match node {
            Node::Group(Kind::Paragraph, children) | Node::Group(Kind::Html, children) => out.push(
                paragraph_from(children, 0, depth, quote, false, &mut prefix),
            ),
            Node::Group(Kind::Heading(level), children) => out.push(paragraph_from(
                children,
                *level,
                depth,
                quote,
                false,
                &mut prefix,
            )),
            Node::Group(Kind::Code, children) => {
                out.push(paragraph_from(children, 0, depth, quote, true, &mut prefix))
            }
            Node::Group(Kind::Quote, children) => {
                collect(children, depth, true, prefix.take(), out)
            }
            Node::Group(Kind::List(first), children) => {
                let mut index = first.unwrap_or(1);
                for item in children {
                    if let Node::Group(Kind::Item, parts) = item {
                        let label = if first.is_some() {
                            format!("{}. ", index)
                        } else {
                            "• ".into()
                        };
                        collect(parts, depth + 1, quote, Some(label), out);
                        index += 1;
                    }
                }
            }
            Node::Group(Kind::Table, children) => {
                let mut rows = Vec::new();
                for row in children {
                    if let Node::Group(kind @ (Kind::Head | Kind::Row), cells) = row {
                        let mut values = Vec::new();
                        for cell in cells {
                            if let Node::Group(Kind::Cell, content) = cell {
                                let mut runs = Vec::new();
                                inline(
                                    content,
                                    &Run {
                                        bold: matches!(kind, Kind::Head),
                                        ..Run::default()
                                    },
                                    &mut runs,
                                );
                                values.push(runs);
                            }
                        }
                        rows.push(Row {
                            header: matches!(kind, Kind::Head),
                            cells: values,
                        });
                    }
                }
                out.push(Block::Table {
                    rows,
                    source: false,
                });
            }
            Node::Rule => para(out, 0, "────────────────────────"),
            _ => {}
        }
        start = index + 1;
    }
    if start < nodes.len() {
        out.push(paragraph_from(
            &nodes[start..],
            0,
            depth,
            quote,
            false,
            &mut prefix,
        ));
    }
}

fn sections(report: &Value, department: bool) -> Vec<Block> {
    let mut blocks = Vec::new();
    let zone = if s(report, "timeZone").is_empty() {
        "Asia/Shanghai"
    } else {
        s(report, "timeZone")
    };
    let title = if department {
        format!("{} · 部门当日工作汇总", s(report, "department"))
    } else {
        format!("{} · 当日工作汇报", s(report, "memberName"))
    };
    para(&mut blocks, 9, title);
    para(
        &mut blocks,
        1,
        if department {
            "基本信息"
        } else {
            "基本信息与采集范围"
        },
    );
    para(
        &mut blocks,
        0,
        if department {
            format!(
                "部门：{}　日期：{}　版本：V{}",
                s(report, "department"),
                s(report, "day"),
                number(report, "version")
            )
        } else {
            format!(
                "部门：{}　成员：{}　日期：{}",
                s(report, "department"),
                s(report, "memberName"),
                s(report, "day")
            )
        },
    );
    para(
        &mut blocks,
        0,
        format!(
            "报告编号：{}　版本：V{}",
            s(report, "id"),
            number(report, "version")
        ),
    );
    if department {
        para(
            &mut blocks,
            0,
            format!("生成时间：{}", timestamp(s(report, "createdAt"), zone)),
        );
    } else {
        para(
            &mut blocks,
            0,
            format!(
                "采集截至：{}（{}）　会话数：{}",
                timestamp(s(report, "capturedAt"), zone),
                zone,
                number(report, "sessionCount")
            ),
        );
        para(
            &mut blocks,
            0,
            format!(
                "来源版本：V{}　状态：{}",
                number(report, "snapshotVersion"),
                if s(report, "finalizedAt").is_empty() {
                    "待定稿".into()
                } else {
                    format!("已定稿 {}", timestamp(s(report, "finalizedAt"), zone))
                }
            ),
        );
        if let Some(warnings) = report["warnings"].as_array() {
            for warning in warnings {
                if let Some(text) = warning.as_str() {
                    para(&mut blocks, 0, text);
                }
            }
        }
    }
    if !report["body"].is_string() {
        para(
            &mut blocks,
            0,
            format!(
                "提示词：{}　模型：{}",
                s(report, "promptVersion"),
                s(report, "model")
            ),
        );
    }
    if let Some(body) = report["body"].as_str() {
        blocks.extend(markdown(body));
    } else {
        let headings: &[(&str, &str)] = if department {
            &[
                ("overview", "一、总体进展"),
                ("members", "二、成员工作摘要"),
                ("blockers", "三、共同阻塞"),
                ("nextSteps", "四、后续安排"),
            ]
        } else {
            &[
                ("tasks", "一、当日任务"),
                ("outputs", "二、实际执行与产出"),
                ("verification", "三、验证情况"),
                ("blockers", "四、未完成事项与阻塞"),
                ("nextSteps", "五、后续计划"),
            ]
        };
        for (key, heading) in headings {
            para(&mut blocks, 1, *heading);
            let findings = report["content"][*key].as_array();
            if findings.is_none_or(Vec::is_empty) {
                para(&mut blocks, 0, "记录中无可确认内容");
            }
            if let Some(findings) = findings {
                for finding in findings {
                    let label =
                        if department && finding["personalStatement"].as_bool().unwrap_or(false) {
                            "【成员陈述，未经独立核验】".into()
                        } else if !s(finding, "status").is_empty() {
                            format!("【{}】", s(finding, "status"))
                        } else {
                            String::new()
                        };
                    para(&mut blocks, 0, format!("{}{}", label, s(finding, "text")));
                    let ids = finding[if department {
                        "reportIds"
                    } else {
                        "evidenceIds"
                    }]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("、")
                    })
                    .unwrap_or_default();
                    if !ids.is_empty() {
                        para(
                            &mut blocks,
                            0,
                            format!("来源{}：{}", if department { "日报" } else { "" }, ids),
                        );
                    }
                }
            }
        }
    }
    if let Some(sources) = report["sources"]
        .as_object()
        .filter(|sources| !sources.is_empty())
    {
        para(
            &mut blocks,
            1,
            if department {
                "来源日报版本"
            } else {
                "来源索引"
            },
        );
        let mut rows = vec![Row {
            header: true,
            cells: vec![plain("来源标识"), plain("记录位置／日报版本")],
        }];
        for (id, value) in sources {
            rows.push(Row {
                header: false,
                cells: vec![
                    plain(id.clone()),
                    plain(
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string()),
                    ),
                ],
            });
        }
        blocks.push(Block::Table { rows, source: true });
    }
    if department {
        para(&mut blocks, 1, "未纳入汇总的成员");
        let missing = report["missingMembers"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        para(
            &mut blocks,
            0,
            if missing.is_empty() {
                "无".into()
            } else {
                missing
            },
        );
    } else {
        para(&mut blocks, 1, "个人补充说明");
        if report["feedback"].is_object() {
            let feedback = &report["feedback"];
            para(
                &mut blocks,
                0,
                format!(
                    "作者：{}　提交时间：{}　正文版本：V{}",
                    s(feedback, "author"),
                    timestamp(s(feedback, "submittedAt"), zone),
                    number(report, "version")
                ),
            );
            para(
                &mut blocks,
                0,
                if feedback["noSupplement"].as_bool().unwrap_or(false) {
                    "无补充说明"
                } else {
                    s(feedback, "text")
                },
            );
        } else {
            para(&mut blocks, 0, "等待成员补充说明");
        }
    }
    blocks
}

fn html_runs(runs: &[Run]) -> String {
    let mut out = String::new();
    for run in runs {
        let mut text = xml(&run.text)
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\n', "<br>");
        if run.code {
            text = format!("<code>{text}</code>");
        }
        if run.bold {
            text = format!("<strong>{text}</strong>");
        }
        if run.italic {
            text = format!("<em>{text}</em>");
        }
        if run.strike {
            text = format!("<s>{text}</s>");
        }
        if run.link {
            text = format!("<span class=link>{text}</span>");
        }
        out.push_str(&text);
    }
    if out.is_empty() {
        "&#160;".into()
    } else {
        out
    }
}
fn html_document(title: &str, blocks: &[Block]) -> String {
    let mut out = format!(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'"><title>{}</title><style>
@page{{size:A4;margin:25.4mm;@bottom-center{{content:'第 ' counter(page) ' 页 / 共 ' counter(pages) ' 页';font:9pt 'SimSun','宋体',serif;color:#666}}}}
*{{box-sizing:border-box}}html,body{{margin:0;padding:0}}body{{font:11pt 'SimSun','宋体',serif;color:#333;line-height:1.35}}h1,h2,h3,h4{{font-family:'Microsoft YaHei','微软雅黑',sans-serif;color:#1f3864;break-after:avoid;page-break-after:avoid}}h1{{font-size:20pt;margin:0 0 20pt;font-weight:normal}}h2{{font-size:14pt;margin:16pt 0 8pt}}h3{{font-size:13pt;margin:14pt 0 8pt}}h4{{font-size:12pt;margin:12pt 0 8pt}}p{{white-space:pre-wrap;overflow-wrap:anywhere;margin:0 0 8pt;orphans:3;widows:3}}.quote{{border-left:2pt solid #b9c5d5;padding-left:10pt;color:#555}}.code{{padding:8pt;background:#f4f5f7;white-space:pre-wrap}}code{{font:10pt Consolas,'SimSun',monospace;white-space:pre-wrap;overflow-wrap:anywhere}}.link{{color:#245b8a;text-decoration:underline}}table{{width:100%;border-collapse:collapse;table-layout:fixed;margin:0 0 12pt}}th,td{{border:.5pt solid #d9e0e8;padding:7pt;vertical-align:top;white-space:pre-wrap;overflow-wrap:anywhere;font-weight:normal}}th{{background:#eef2f7;font-weight:bold}}td p,th p{{margin:0}}thead{{display:table-header-group}}tr{{break-inside:auto}}tbody{{break-inside:auto}}
</style></head><body>"#,
        xml(title)
    );
    for block in blocks {
        match block {
            Block::Paragraph(p) => {
                let tag = match p.level {
                    9 => "h1",
                    1 => "h2",
                    2 => "h3",
                    3 => "h4",
                    _ => "p",
                };
                let class = if p.code {
                    "code"
                } else if p.quote {
                    "quote"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "<{tag} class=\"{class}\" style=\"margin-left:{}pt\">{}</{tag}>",
                    p.indent * 18,
                    html_runs(&p.runs)
                ));
            }
            Block::Table { rows, source } => {
                out.push_str("<table>");
                if *source {
                    out.push_str(
                        "<colgroup><col style=\"width:32%\"><col style=\"width:68%\"></colgroup>",
                    );
                }
                let mut body = false;
                for row in rows {
                    if row.header {
                        out.push_str("<thead>");
                    } else if !body {
                        out.push_str("<tbody>");
                        body = true;
                    }
                    out.push_str("<tr>");
                    for cell in &row.cells {
                        let tag = if row.header { "th" } else { "td" };
                        out.push_str(&format!("<{tag}><p>{}</p></{tag}>", html_runs(cell)));
                    }
                    out.push_str("</tr>");
                    if row.header {
                        out.push_str("</thead>");
                    }
                }
                if body {
                    out.push_str("</tbody>");
                }
                out.push_str("</table>");
            }
        }
    }
    out.push_str("</body></html>");
    out
}

pub fn render(report: &Value, department: bool) -> Result<String> {
    ensure!(report.is_object(), "报告内容格式无效");
    let blocks = sections(report, department);
    let title = if department {
        format!("{} · 部门当日工作汇总", s(report, "department"))
    } else {
        format!("{} · 当日工作汇报", s(report, "memberName"))
    };
    Ok(html_document(&title, &blocks))
}
