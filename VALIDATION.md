# 0.6.0 Trae 导出导入

日期：2026-11-12。成员端新增 Trae 手动导出导入来源；组长端协议、Agent 与提示词未改。Trae 本地加密数据库不直接读取，自动解析待真实样本。

本轮验证路径：

- 解析回归：成员端 15 项、组长端 16 项库回归通过（`cargo test --lib`、`--features leader`）。新增 6 项覆盖 Markdown 角色段落、JSON 消息数组、JSONL 逐行、无时间戳按文件日期、跨日导出跳过、无法识别文件告警；样例中核对系统段落/导出元数据剔除与凭据脱敏。
- 隔离目录实际 HTTPS 验收（`cargo run --features leader --example verify_trae_import`）：成员端启用 Codex + Trae 导出两个来源，采集得到 2 个会话；Trae 会话标题、事件数与预览正确，系统段落和导出时间元数据未进入预览；选择后上传，组长端协议校验通过，快照 4 条事件、来源摘要含 codex/trae，且不含 Trae 系统段落与导出元数据。结果：`artifacts/acceptance/trae-import/latest-result.json`。
- 既有回归：`verify_multi_source`（Codex+DSH 上传）、`verify_selection`（选择/离线/空记录）通过；前端 TypeScript/Vite 构建通过。
- 交付产物（本机重建，不进入源码仓库）：Windows 成员端安装包 1,892,759 字节（SHA-256 `91DD092D…F468309987`），组长端 27,912,247 字节（`C81CFB38…C23683B`），Ubuntu 成员端 deb 3,959,930 字节（`1CFCB60D…B9E10129`）；源码包校验值见 `artifacts/installers/SHA256.txt`。Windows 发布目录双端 0.6.0 可执行文件以隔离 `--data-dir` 实际启动，数据目录与数据库/证书创建正常、无启动错误（`artifacts/acceptance/runtime-0.6/result.json`）；deb 在 WSL Ubuntu 22.04 内用 `apt install` 实际安装（依赖自动补齐，desktop 入口 `Categories=Office`），已安装二进制无头启动正常、数据目录 0700，`apt remove` 后成员数据保留。
- 边界：未获得真实 Trae 导出样本，解析规则基于常见 Markdown/JSON 对话结构；导入内容可在预览核对，仍可能对特殊格式产生误判。未实现读取 Trae 本地加密数据库；本轮未调用模型、未修改生产数据。

# 0.5.0 Ubuntu 成员端

日期：2026-11-12。本次只变更成员端构建与平台层；组长端仍为 Windows x64，Windows 成员端行为与 0.4.0 相同。

本轮验证路径（WSL2 Ubuntu 22.04.2 LTS，webkit2gtk-4.1 2.50.4，Rust stable，Node 22）：

- 库回归与构建：安装 Tauri Linux 依赖后 `npm ci`、`npm run build`、`cargo test --manifest-path desktop/src-tauri/Cargo.toml --lib`（9 项采集回归全部通过）与 `cargo build --release` 通过；未启用 `leader` feature（组长端功能仅 Windows）。
- 打包：`npx tauri build --bundles deb --config src-tauri/tauri.build.conf.json` 生成 `CodexDailyReport Member_0.5.0_amd64.deb`（交付包 3,937,394 字节，SHA-256 `cb7e3ec97c7ebe148a43b202c953920218463d74bd47c67a9945e9d892f07486`）。控制信息：`Package: codex-daily-report-member`、`Version: 0.5.0`、`Architecture: amd64`，依赖含 `libwebkit2gtk-4.1-0`、`libgtk-3-0`、`libayatana-appindicator3-1`、`libssl3`、`xdg-utils`、`fonts-noto-cjk`；文件列表含 `/usr/bin/CodexDailyReport.Member`、desktop 入口、`Categories=Office;` 与 32/128/256/512 图标。
- 无头启动：Xvfb + dbus-run-session 下以 `--data-dir` 启动，进程持续运行超过 12 秒；数据目录自动创建且权限为 0700，无 `startup-error.txt`，本环境托盘构建成功、无托盘失败提示。环境内 xdg-desktop-portal 的 PipeWire 警告与 GTK 无窗口管理器提示不影响启动。
- 单实例：同一数据目录启动第二个实例，第二个进程返回 0 后退出，第一个实例继续运行；再次启动恢复已隐藏窗口的单实例插件分支可用。
- Windows 回归：平台层拆分后 Windows `cargo test --lib`（9 项）与 `--features leader`（10 项，含内置提示词解密）通过；Windows 双端 0.5.0 Release 与 NSIS 安装包重建通过。
- Windows 运行验证：`artifacts/tauri-publish` 下 0.5.0 成员端与组长端可执行文件分别以隔离的 `--data-dir` 启动，10 秒后进程存活；成员端创建 `outbox`，组长端创建 `department.db`、`submissions`、`reports` 与证书文件，均无 `startup-error.txt`；验证后结束测试进程，未触碰已安装程序与真实部门数据。证据：`artifacts/acceptance/runtime-0.5/result.json`。
- 包安装、重装与卸载：把交付的 `CodexDailyReport.Member.deb`（SHA-256 `CB7E3EC9…7486`）在 WSL 内用 `apt install` 实际安装成功，`dpkg -s` 为 `install ok installed`、版本 0.5.0、架构 amd64，依赖由 apt 自动补齐；`/usr/bin/CodexDailyReport.Member`、desktop 入口（`Categories=Office;`）与图标就位。`apt install --reinstall` 后已安装的二进制在 Xvfb + dbus 下以 `--data-dir` 启动，进程持续运行、数据目录权限 0700。`apt remove` 后包状态移除但成员数据目录保留。
- 交付产物（本机重建，均不进入源码仓库）：Windows 成员端安装包 1,876,123 字节（SHA-256 `38A1E87E31FAF41BCBB6421D66241278A0A53408E6179561F3BA12CA07AEC3DD`），组长端 27,920,012 字节（`F71E3E5109D07E68A01DF107116A078926352B4A7852CD0ED841BA2FC7B1D0C7`），源码包 304,241 字节（`DF4F462E56E9E3F29FF85EC273430C9D67FA4E8CB744BCF0E37E8CE661BB1A56`），Ubuntu 成员端 deb 3,937,394 字节（`CB7E3EC97C7EBE148A43B202C953920218463D74BD47C67A9945E9D892F07486`）。
- Trae 探测工具自测：在合成目录树上（明文 SQLite 文件头、随机字节的加密样本、`-IncludeStateDb`）运行 `scripts/probe-trae.ps1 -ExtraRoots`，正确分类 `sqlite-plaintext` / `encrypted-or-unknown` 并复制 3 个库文件，输出 `trae-probe.json`。仅使用合成数据，未涉及真实 Trae 安装或数据。证据：`artifacts/acceptance/runtime-0.5/trae-probe-selftest`。
- 跨端协议验证（本机沙箱内 WSL 无法访问 Windows 主机端口：Windows 防火墙拦截入站，且 WSL 镜像网络模式配置失败，故拆成两端分别验证）：
  1) Linux 成员端在 WSL 内对**真实 HTTPS 监听器**（示例内置协议对端，按 v1 路由实现 `/v1/pair`、`/v1/status/{day}`、`/v1/submissions`、`/v1/supplements` 与 Bearer 校验）完成配对、Codex+DSH 双来源采集、上传与状态查询；正确指纹成功、错误指纹在 TLS 阶段被拒绝；上传的 capture 经服务端协议版本、时区、哈希、事件类型与会话计数校验；DSH 内部记录未进入上传内容。证据：`examples/verify_member_transport`（Linux-only）与 `artifacts/acceptance/linux-transport/result.json`。
  2) 把 Linux 端采集并上传的 capture JSON 交给**真实 Windows 组长端**的 `leader.validate_capture` 校验，结果通过（2 会话、6 事件、协议 1、时区与事件时间范围一致，来源摘要含 codex/dsh）。证据：`examples/verify_capture_file` 与 `artifacts/acceptance/cross-platform/leader-validation.json`。
- 边界：跨端**网络级实机配对**仍未验证（此环境 WSL↔Windows 网络被拦截，见上）；未在真实 Ubuntu 桌面（X11/Wayland、GNOME 托盘扩展、HiDPI）上人工操作；未覆盖 ARM 和 Ubuntu 20.04；Linux 端出现 xdg-desktop-portal 的 PipeWire 警告，属无头环境提示。

# 0.4.0 多来源采集（Codex + DeepSeek Harness）

日期：2026-11-12。成员端采集与组长端提示词更新。需求来自用户确认：Linux 版本只做成员端；Trae 安装在 Windows、样本待提供；DSH 部门成员普遍使用；单条记录设上限；顺序为 0.4 多来源、0.5 Linux 成员端、Trae。

本轮验证涉及的路径：

- 采集层回归（9 项库测试，`cargo test --lib` 与 `--features leader` 均通过）：Codex 既有结构、脱敏、跨日、归档与损坏行行为保持；Codex 新增超长事件截断与修改时间预筛；DSH v4 可见事件映射与内部记录排除（system/message、request/header、注入型 user/message、title LLM、delivery）；DSH 继承会话按 `session/end-seed` 截点跳过历史、缺少截点时整会话跳过并告警；旧/较新格式提示；损坏尾部容错；跨日过滤；超长工具结果截断标注。
- 真实本机数据只读探针（`cargo run --example verify_sources -- --dsh %USERPROFILE%\.dsh --codex %USERPROFILE%\.codex`）：2026-11-12 当日 6 个会话（Codex 1、DSH 5）、1083 条事件，采集耗时约 1.1 秒；结构化核对 DSH 内部记录（系统提示、请求头、会话日志投递）进入采集的数量为 0；未上传、未写入报告、未修改本机会话。结果：`artifacts/acceptance/multi-source/probe-20261112-*.json`（本机副本，不进入发布包）。
- 隔离目录实际 HTTPS 多来源闭环（`cargo run --features leader --example verify_multi_source`）：成员端分别配置 Codex 与 DSH 来源，采集得到 2 个会话；DSH 会话标题、项目、来源和子代理字段保留，预览包含可见对话/工具结果且不含系统提示、推理与注入上下文；选择两类会话后经实际 HTTPS 上传，组长端协议校验通过，快照为 2 会话 6 事件，事件带 `tool`、快照带 `sources` 摘要，内部记录未出现。结果：`artifacts/acceptance/multi-source/latest-result.json`。
- 既有选择流程回归（`verify_selection`）：新的来源配置操作下，选择、冻结快照、离线排队、重复确认、排除会话与空记录语义全部保持；会话选择记忆仍以 Codex 路径为键。结果：`artifacts/acceptance/tauri-selection/latest-result.json`。
- TypeScript/Vite 构建、`cargo check --examples --features leader`、Windows 双端 Release 与 NSIS 安装包构建通过（版本 0.4.0，产物与校验见 `artifacts/installers/SHA256.txt`）。

未验证与边界：本轮不调用外部模型，`personal.v3`、`extract.v2` 的提示词改动只验证了资源加密/解密与构建，未在实际模型上生成报告；Trae 本地记录未接入（其 Windows 安装目录与导出样本待用户提供）；Linux 成员端为 0.5 范围；真实 DSH 探针只读取当日本机会话，不代表其他成员机器与历史格式的覆盖。未修改生产数据、未上传真实工作记录。

# 0.3.6 成员补充版本冲突

日期：2026-11-10。仅更新成员端，组长端沿用 0.3.5。组长拒绝旧正文或旧来源的补充符合版本规则；缺陷是成员界面仍允许提交，且旧版队列只显示 HTTP 409，未给出可继续操作的路径。

本轮只验证两个相关路径：

- 真实 MemberClient → HTTPS → Leader：成员先读取有效报告，组长来源更新后提交旧报告补充，实际收到版本冲突。确认旧报告未写入反馈，原始拒绝请求加密留存、草稿保留、不再排队重发。继续迁移旧版已失败409队列，确认较新编辑稿不被旧请求替换；下发新版后带入草稿但不自动确认，成员明确提交后仅新版本收到原文。结果：`artifacts/acceptance/feedback-conflict/result.json`；复现：`cargo run --manifest-path desktop/src-tauri/Cargo.toml --features leader --example verify_feedback_conflict`。报告和来源为隔离样例，没有调用模型。
- 实际 Vite 发布界面使用上述 HTTPS 结果作为 IPC 样例：旧报告可读，补充可编辑和保存，旧版提交按钮不可用且给出生成新版提示；自动刷新先于450ms自动保存时，最新输入保留并写入旧/新版本草稿；点击提交等待草稿保存时，即使界面切到新版，仍发送点击时的 reportId 和文本。版本冲突后不显示成功，旧版按钮保持不可用。结果：`artifacts/acceptance/feedback-conflict/ui-result.json`；截图：`stale-feedback.png`，已检查。此项仅验证前端交互，不替代原生桌面全流程验收。

TypeScript/Vite、成员 Rust Release 和 NSIS 安装包构建通过。独立 review 已复核版本绑定、旧/新草稿持久化、409留存与离线队列行为，无剩余已确认阻塞。仅发布成员 0.3.6，组长 0.3.5 安装包哈希不变。未重复模型生成、成员采集、PDF 排版或安装卸载验证，未修改生产数据。

# 0.3.5 长记录分段、并行与续跑

日期：2026-11-09。仅更新组长端，成员端安装包保持 0.3.3。短记录直接成稿；长记录按会话/事件分段提取证据，按容量归并，再对照原始提取事实检查跨会话修正及验证范围，统一生成 Markdown。增加请求名额、各片进度、限流等待、局部重试和来源范围记录。

本轮仅验证改动涉及的路径：

- 真实失败样例：读取此前因上下文超限失败的已上传快照，在独立部门副本使用配置模型生成。共 9 个会话、2,683 条事件、9,500,500 个序列化字符，完整消息估算 3,542,611 Token，分为 87 个原文片段。逐事件检查原文字符范围，无缺口或重复主覆盖；边界上下文单独标记。证据：`artifacts/live/segmented-035/20261109-133802/coverage-audit.json`。
- 实跑续跑：首轮成功 4 段后受同 Key 其他请求影响而限流，续跑复用成功缓存并完成。人工抽查发现后续时区修正被归并遗漏，增加跨会话核对后复用 87 段提取及 7 次归并结果，执行 5 批核对再成稿。核对阶段 3 批成功、2 批限流；修正短 Retry-After 覆盖本地退避的问题后，同任务续跑只发出 2 批剩余核对及 1 次成稿请求，67 秒完成。最终进度 100/100，保留 2,818 项内部来源定位，报告自动下发至隔离副本。首版和修正版均保留，见 `first-pass`、`personal.md`、`job.json` 和 `reconciliation-resume.log`。
- 人工内容核对：修正版已写明时区映射代码已补充而专项运行测试未见证据；区分代理 TLS 探测与真实模型对话、RAG 说明修改与服务恢复、课件构建与最终视觉复审。原文覆盖检查不等于每条语义均正确，未据此宣称模型不会遗漏。没有修改生产数据、代替成员确认或导出该真实样例 PDF。
- Rust 任务宿主实际子进程验证：两个任务共享请求名额，峰值 4；占用及排队取消释放名额，另一任务继续；失败后相同任务重试成功，无半截报告，详细进度及分析来源持久化。结果：`artifacts/acceptance/tauri-slots/latest-result.json`，取消释放当次为 29 毫秒。
- 实际 Vite 发布界面验证分片计数/错误、刷新保留滚动位置、等待重试、取消、任务重试及容量配置保存。结果：`artifacts/acceptance/segmented-ui/result.json`、`progress.png`。IPC 使用隔离样例，仅覆盖前端交互。
- 最终冻结 Agent 通过本地 HTTP 流式接口验证分段并行、完整范围、明确超限后细分、成功片复用、原文回查、来源修复、完成检查点恢复，以及部门作者/正文版本/原文补充归属；同时检查短记录成稿、显式/静默断流重试、残缺输出不发布和永久错误回显。外部 HTTP/HTTPS 代理设为不可达，本地接口除外，随包分词器正常加载，无运行时下载。结果及最终运行时 SHA-256：`artifacts/acceptance/segmented-runtime/result.json`；日志：`artifacts/packaged-pipeline-035.log`、`artifacts/packaged-direct-035.log`。

TypeScript/Vite、Rust Release、Python Agent 及组长 NSIS 安装包构建通过。独立 review 已复核来源恢复、回查容量、嵌套检查点、跨会话核对、限流退避及取消名额释放，无剩余已确认阻塞。未重复成员采集、网络发现、安装卸载或 PDF 排版回归。

# 0.3.4 PDF 导出与生成取消

日期：2026-11-09。仅更新组长端，成员端保持 0.3.3。个人日报、部门汇总和历史汇总导出统一为 PDF；删除 Word 渲染、归档依赖及复制路径。新增排队/生成中任务的取消操作，取消后保留已有报告，再次生成创建新任务。

本轮只验证变更涉及的路径：

- 原生 WebView2 生成个人与部门 PDF，核对导出文件与归档字节一致、无 Word 文件或返回字段，以及仅有 PDF 时重复导出复用归档。结果：`artifacts/acceptance/tauri-pdf-only/result.json`。使用已有软件验证报告，不重新请求模型；HTML 模板及排版未改。
- 打包 Agent 连接本地流式模拟接口，验证个人/部门任务排队取消不发送请求、生成中取消关闭连接、其他并行任务完成、旧报告保持原样且没有半截报告、重复取消幂等、再次生成成功、已完成任务保留完成结果及重启后取消状态保留；同时核对明确退出时先关闭活动进程连接再回到排队。结果：`artifacts/acceptance/tauri-cancel/latest-result.json`。最终一次场景共 5 个本地模拟请求，直接运行 Rust 任务调度及实际 Agent 子进程，不使用外部模型或生产数据。
- 实际 Vite 发布界面在 Chromium 中点击任务详情、成员行、部门汇总的取消入口；核对已取消状态、停止显示实时活动、保留已有汇总预览、个人/部门 PDF 导出调用及历史汇总 PDF 文案。结果：`artifacts/acceptance/tauri-cancel-ui/result.json`、`cancelled-job.png`。IPC 为隔离样例，验证前端交互，未将其视为原生 Windows 全流程验收。

TypeScript/Vite 构建通过。独立 review 发现并修正同 ID 重试时旧任务可能清理新任务句柄的竞态，按任务句柄身份匹配后再清理；复核无剩余阻塞。未重复采集、网络发现、安装卸载及长文排版回归。

# 0.3.3 上传前会话筛选

日期：2026-11-09。成员端采集后先展示会话列表和脱敏快照，确认时只上传所选会话；同日记住选择，后续新发现的会话默认不选。兼容组长端 0.3.2，仅更新成员端。

本轮必要验证：

- 实际 MemberClient → HTTPS → Leader 闭环：采集、预览和保存选择不上传；排除会话的正文、标题、项目路径及告警不进入队列或组长材料；预览后追加的事件和新会话不混入已确认快照。组长离线时重复确认只产生一项队列，两端重启后同一份筛选材料只形成一个提交版本。继续核对同日选择恢复、新会话默认不选、正常筛选不误报采集异常，以及全不选和未发现记录两种情况的说明。结果：`artifacts/acceptance/tauri-selection/latest-result.json`。复现：`cargo run --manifest-path desktop/src-tauri/Cargo.toml --features leader --example verify_selection`。
- 实际 Vite 发布界面在 Chromium 中完成勾选、搜索、项目过滤、完整脱敏预览、工具折叠展开、取消、重新采集及准确草稿提交；1120×820 和 800×600 截图已查看，底部操作可见，无页面错误。结果：`artifacts/acceptance/tauri-selection/ui-result.json`、`session-picker.png`、`session-picker-small.png`。IPC 使用上项隔离流程样例，覆盖前端交互，不代表原生 Windows 窗口完整验收。
- 采集器提取改动只运行现有 `migrated_collector_keeps_visible_cross_day_and_archived_records` 聚焦回归，覆盖跨日、归档、可见子代理消息和写入中日志，已通过。TypeScript/Vite 构建通过，独立子代理 review 无阻塞。

记录均为隔离的合成样例，没有调用模型或修改生产数据；未重复模型、Word/PDF、网络发现及安装卸载回归。

# 0.3.2 自动下发与直接预览

日期：2026-11-09。保留个人日报生成成功时写入 `releasedAt` 的自动下发流程，移除组长主工具栏中容易误导的手动下发入口，任务完成说明明确显示已自动下发。任务与生成报告在同一事务保存 `reportId`，完成任务入口预览准确的当次版本；旧任务缺少关联时标为预览当前版本。

仅运行两项相关验证：

- 本地模拟 Chat Completions 接口返回一份明确标注的样例正文，实际打包 LangGraph Agent → Rust 完成任务 → HTTPS 成员端读取报告，全程没有手动下发。核对任务关联报告 ID、反馈/定稿前可读取预览内容及补充原文保留。只有一次模拟请求，没有调用外部模型、导出文档或修改生产数据。结果：`artifacts/acceptance/tauri-preview/latest-result.json`。
- 在 Chromium 中运行实际 Vite 发布界面，以前一项结果作为 Tauri IPC 样例，实际点击成员行预览、选中预览、任务预览及汇总入口，验证最新版本与任务对应旧版区分、版本切换、表格、只读正文和原文补充。页面无脚本错误，个人与汇总截图已查看。结果：`artifacts/acceptance/tauri-preview/ui-result.json`、`personal-preview.png`、`summary-preview.png`。这项验证覆盖前端实际交互，未把模拟 IPC 当作 Windows 桌面完整验收。

TypeScript/Vite 构建通过，独立子代理 review 无阻塞。没有重复网络发现、模型重试、Word/PDF 或安装卸载回归。

# 0.3.1 自动识别 IP

日期：2026-11-09。本轮只修改组长端地址发现、网络选择和接收监听。Windows 网卡接口按已连接物理网卡优先，保留 VPN 候选，排除环回、APIPA 和代理使用的 198.18/15；自动模式在初始化、保存和创建邀请时刷新地址，手动地址保持不变。接收服务监听全部 IPv4 接口。

一个聚焦网络场景已完成：首次自动配置 → 模拟旧版 localhost 配置并重启 → 自动迁移 → 实际成员客户端使用检测出的内网 IP 完成 TLS 指纹配对、状态读取和一条样例记录上传 → 创建邀请时更新旧 IP → 确认手动主机名不会被覆盖。没有调用模型，也未改生产数据。

本机当次首选为 WLAN `10.12.39.47`，与 Windows 网卡配置一致；Radmin、NetBird、Tailscale 等候选排在其后。结果：`artifacts/acceptance/tauri-network/latest-result.json`。复现命令：`cargo run --manifest-path desktop/src-tauri/Cargo.toml --features leader --example verify_network`。

TypeScript/Vite 构建及 Rust 检查通过；独立子代理 review 未发现阻塞。网络实测通过同机实际内网地址完成，并不代表组员另一台电脑的防火墙路径已经验证。本次未重复模型、文档导出和安装卸载回归。

# 0.3.0 Tauri 重构验证

日期：2026-11-09。平台：Windows x64。以下为本次重构的验证结果；后续 0.2.x 章节为历史记录，不代表新版界面已完成交互验收。

两端采用 Tauri 2、TypeScript 和 Rust，组长保留现有 Python/LangGraph Agent。Release 与 NSIS 打包完成；成员安装包 1,759,207 字节（1.76 MB），组长安装包 26,204,192 字节（26.20 MB）。成员仅安装主程序，组长附带 Agent；均不携带 .NET、Playwright 或独立 Chromium。

## 本次必要验证

- 采集迁移仅保留一个聚焦场景，覆盖跨日旧会话、归档、可见子代理消息、加密消息跳过、去重及写入中的 JSONL。前端 TypeScript/Vite 构建及两端 Rust Release 构建完成。
- 实际 MemberClient 通过 HTTPS 指纹配对、采集上传和接收确认；组长离线后队列落盘，两端重建后恢复上传且不重复生效。结果：`artifacts/acceptance/tauri-member/latest-result.json`。
- Rust 服务完成配对、幂等接收、补充、归档访问、重新开启与备份恢复验证；只读核对原 C# 报告序列化哈希兼容。独立 review 发现并修正旧归档精确匹配、服务内部错误返回 500 以便队列重试、Windows 时区名称兼容问题。
- 在独立目录，以明确标注的软件验证记录调用配置的实际模型，完成个人日报、HTTPS 提交“无补充说明”、部门汇总，汇总来源状态有效。实际模型调用不重复，证据：`artifacts/acceptance/tauri-live/run.json`、`personal.json`、`summary.json`。
- 实际通过系统 WebView2 生成个人、部门和长文 PDF，同源生成 DOCX。个人及部门各 1 页，长文 22 页；核对中文、跨页表格、长路径、页码及原文补充，检查首末页渲染与 Word XML。证据：`artifacts/acceptance/tauri-documents/result.json`、`layout.json`。
- 两个正式 NSIS 安装包均静默安装到含中文和空格的自选目录，再执行卸载。成员 1 个、组长 365 个发布文件匹配；主程序仅规范化 Tauri 安装时写入的 UNK→NSS 类型标记，其余字节相同。旧 WPF 安装注册位置不变。证据：`artifacts/acceptance/tauri-installers.json`。

成员端可访问性结构确认正文为只读区域、补充为可编辑控件，且无模型配置。电脑随后处于锁屏状态，停止窗口交互；两端最终视觉、输入和安装器目录选择页面尚未实际点击复核。上述非界面业务与静默安装验证已完成，未修改生产数据。

## 历史：0.2.2 实际使用验证

日期：2026-11-09。平台：Windows x64。

## 0.2.2 安装位置

两端安装器支持编辑安装路径或使用文件夹选择器；默认从当前用户安装注册项读取上次位置。程序快捷方式、启动入口和卸载脚本使用所选路径。日报数据仍使用原用户数据目录。

本轮只验证安装变更：两个实际安装包均安装到包含中文与空格的自选目录，逐文件比对发布产物后覆盖升级，再执行生成的卸载脚本。安装、升级及卸载成功，现有生产安装注册位置未改变；选中包含其他文件的目录时原文件保持不变。结果位于 `artifacts/acceptance/installer-location-verification.json`。

构建版本为 0.2.2，零编译警告和错误。报告、采集和 Agent 代码未变化，未重复模型调用或业务回归。可用 `pwsh -File scripts/verify-installers.ps1 -SkipRuntimeChecks` 重现本次安装路径检查。

## 0.2.1 实跑问题与修复

原始提交 `1094430e66c24c108d4b4c0982395c80` 有 421 条事件，正文约 151 万字符。生产任务在已返回约 890 字符后收到上游错误 `stream error: stream disconnected before completion: stream closed before response.completed`，软件未自动重试且只显示 `APIError`。

现使用 `ModelRetryMiddleware` 处理瞬时连接或断流错误，并显示脱敏后的具体原因。SSE 复现覆盖显式错误和未发送完成标记即结束连接两种情况：收到部分正文后中断、再次调用成功，最终正文及检查点只保留完整结果；认证错误不进入此重试。旧检查点缺少完成标记时也不会恢复为正式报告。

采集告警的 34 条原始 `response_item.agent_message` 均已在隔离目录交给实际 C# 采集器核对：8 条可见正文被采集、26 条仅加密正文正常跳过，问题告警为 0。没有将加密块或内部元数据写入采集结果，临时真实日志样本已删除。该格式的聚焦回归并入原成员业务场景。

同一份已提交资料通过实际模型在 `artifacts/live/stream-recovery-021` 独立副本重跑成功：个人日报约 89 秒、部门汇总约 19 秒，Word/PDF 导出与成员下载一致，PDF 各 2 页且全部页面已检查。这次实际接口没有再次断流；自动重试的证据来自上述故障复现，二者分开记录。生产数据未修改。

本轮必要业务回归结果位于 `artifacts/acceptance/run-20261109-012531/result.json`。安装包检查仍使用本文下方的同一脚本。

0.2.1 最终 Release 编译零警告、零错误；成员端与组长端安装包的文件版本均为 0.2.1。隔离安装文件与发布产物匹配，安装后的 Agent 完成显式断流和静默 EOF 恢复验证，内置 Chromium 导出中文 PDF 成功。

## 0.2.0 基线实跑

使用本机已提交的 2026-11-08 记录：3 个会话、159 条事件，正文合计 223,255 个 UTF-16 字符。读取正在使用的数据库时使用只读连接，生成和反馈全部在 `artifacts/live/langgraph-020` 独立副本进行。

通过当前配置的实际 Chat Completions 服务及 `gpt-6.1-terra` 完成：HTTPS 上传 → 个人 Agent 生成 → 成员读取 → 提交验证补充 → 部门 Agent 汇总 → 导出 Word/PDF → 成员下载 PDF 并与导出文件逐字节比较。个人生成约 26 秒，部门汇总约 22 秒；两个生成环节各一次逻辑请求，没有应用层分段归并。结果在 `artifacts/live/langgraph-020/result.json`。

个人和部门 PDF 各 2 页，已检查全部页面、中文标题、正文、页码、补充和来源版本。实际发现的中文相邻加粗符号未解析问题已通过 CJK Markdown 支持修复，重新导出后检查无残留标记。Word 来自同份 Markdown 内容，保留链接目标；个人补充作为原文处理。输出在该目录下的 `exports`，渲染图及文本检查在 `rendered`。

实际成稿区分了只读检查、未执行的变更、未验证的恢复及用户反馈。验证补充明确写明“尚未经员工本人复核”，部门 Agent 也保留该含义。这些文件是软件流程验证副本。

## 必要业务回归

`tests/DailyReport.Tests/Program.cs` 保留四个串联场景，结果写入 `artifacts/acceptance/latest-result.json`：

- 真实回环 HTTPS 上传、离线队列和重启恢复、重复提交只生效一次、生成后自动下发、补充不可为空及成员身份隔离。
- 未导出但已反馈的日报参与汇总，完整正文与补充共同输入，补充改变使汇总过期，导出本身不改变汇总来源，重新开启保留旧归档。
- 独立成员并行进入 Agent，一人失败不影响其他人；来源不变时重试继续原任务。
- Markdown 表格与正文生成 Word、Open XML 结构可读、补充原文保留、PDF 实际导出及成员下载、旧报告序列化兼容。

`agent/smoke_test.py` 使用本地模拟接口检查长输入完整送达，空输出或明确截断/过滤后能重试，已完成任务复用检查点，以及提示词和 Key 不写入检查点。该检查也用于安装后的独立 Agent 可执行文件。

本次没有扩展逐字段、逐状态的单元测试，也没有再次运行旧分段算法的测试。

## 桌面与 review

在独立测试目录打开两个 0.2.0 桌面程序：成员正文只读、补充可实际编辑，无 Agent 进度；组长配置无字符预算，看板无虚构百分比；部门汇总可预览并提供 Word/PDF 导出入口。检查结束仅关闭测试进程。

子代理 review 指出并已修复：空终态重试不执行、上游错误正文可能进入日志、Word 丢失链接目标，以及重试较早任务时首页仍显示较新已完成任务。任务展示现优先显示运行中的任务，再按最近反馈时间排序。

## 安装与可重现命令

```powershell
pwsh -File scripts/build.ps1
pwsh -File scripts/verify-installers.ps1
```

安装验证把两个安装包解包到独立目录并逐文件比对发布产物，确认成员包无 Agent、组长服务或提示词依赖，并实际运行安装后的 Agent 和 Chromium 导出。结果写入 `artifacts/acceptance/installer-verification.json`。隔离安装不修改用户正在运行的软件、开始菜单或卸载注册表。

0.2.0 基线 Release 构建零警告、零错误，隔离安装、打包 Agent 和 Chromium 检查完成，文件比对无差异；对应业务回归结果在 `artifacts/acceptance/run-20261109-005722/result.json`。当前版本的安装结果以 `installer-verification.json` 为准。

实际模型检查需显式执行，重复使用已经完成的验证目录时只复用成稿并导出：

```powershell
.\.tools\dotnet\dotnet.exe run --project tests/DailyReport.Tests -c Release -- --live --live-root "F:\WORKSPACE\codex工作汇总\artifacts\live\langgraph-020"
```

## 尚未覆盖

目标部门多台电脑的内网/VPN、十五人现场试用和长期连续运行尚未验证。安装器隔离测试不等于真实开始菜单、登录启动项与卸载流程验收；安装包尚未使用发布者代码签名。
