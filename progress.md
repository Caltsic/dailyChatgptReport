## 0.7.0 最终回归（交付前复核）

- 成员端 22 项、组长端 25 项、无桌面核心 22 项库回归通过；verify_multi_source / verify_selection / verify_trae_import 三条隔离 HTTPS 验收通过。
- 真实本机 DSH/Codex 只读探针：7 会话、2611 事件、内部记录泄漏 0（约 2.0 秒，不落盘不上传）。
- 前端 TypeScript/Vite 构建通过；ci.yml / release.yml 用 PyYAML 解析通过（CI 5 任务、Release 2 任务）；交付物 SHA-256 与 artifacts/*/SHA256.txt 一致。

## 0.7.0 附：Ubuntu 24.04 成员端界面全流程（WSLg）

- 在本机导入的 Ubuntu 24.04.5 + WSLg 环境中，用 xdotool 驱动交付的 0.7.0 成员端真实界面：绑定部门（X11 剪贴板粘贴邀请）→ 同意书 → 采集并选择会话（Codex/DSH/Trae 3 来源、3 会话/6 记录、脱敏预览）→ 确认上传；界面显示“已收到”。
- 链路：WSL 不能直连 Windows，用 Windows→WSL SSH 反向隧道，邀请地址 `https://127.0.0.1:18511`→Windows 组长 18510，TLS 与指纹端到端；给验收示例加了 `--advertised-port`（仅测试工具）。组长端 `leader.validate_capture` 通过：protocol 1、3 会话/6 事件、codex/dsh/trae 各 2。
- 证据 `artifacts/acceptance/linux-gui-flow/20261112-194826`（01–09 截图、leader/tunnel 日志、environment.txt、复现脚本）。边界：合成数据；验收组长示例写结果后退出，界面出现离线缓存提示；最小镜像缺彩色 emoji 字体。

## 0.7.0 附：Windows 成员端界面全流程（UIA 驱动）

- 用 UI Automation 在真实 Windows 桌面驱动 0.7.0 成员端：绑定部门（ValuePattern 粘贴邀请）→ 同意并绑定 → 采集并选择会话（真实选择器展示 Codex/DSH/Trae 三来源、3 会话/6 记录、Trae 脱敏提示）→ 确认上传 3 个会话；组长端（`verify_cross_platform_leader`，当前源码重建）校验通过（protocol 1、3 会话/6 事件），成员状态 🟢 已收到。
- 发现并修复：此前验收用的组长示例二进制是旧版本，缺 0.6.1 的 `说明：` 分类，导致状态显示“采集异常”；重建后一致。
- 证据 `artifacts/acceptance/gui-flow/20261112-194009`（01–06 截图、UIA 名称、summary/leader 结果、environment.txt）。全部合成数据，无模型调用。

## 0.7.0 附：Windows/Ubuntu 界面与来源对话框验证

- Windows 发布目录成员端（0.7.0，隔离数据目录）：PrintWindow 截窗口 + UI Automation 调用“记录来源”按钮，找到 Codex、DeepSeek Harness、Trae 本地记录、Trae 导出四项；截图与 `uia-check.txt` 在 `artifacts/acceptance/windows-ui-0.7.0`。本机检测到 `C:\Users\Kaltsit\.codex`、`.dsh`，Trae 显示未安装提示。
- Ubuntu 24.04 WSLg：xdotool 点击“记录来源”后截弹窗，同样显示四项来源（`member-source-dialog-ubuntu2404.png`）。
- 边界：无 Trae 安装/样本、未调用模型；Windows NSIS 安装/卸载未在本机执行（用发布二进制）。

## 0.7.0 附：Ubuntu 24.04 桌面窗口（WSLg）

- 用户工作区 24.04 无 sudo/WebKitGTK，改用本机 WSL2 导入独立 Ubuntu 24.04.5 cloud-image 发行版（F:\WSL\Ubuntu2404，非 22.04 构建环境），WSLg 1.0.71（Wayland + XWayland）。
- 交付的 0.7.0 deb 用 apt 安装成功（自动补 WebKitGTK 4.1 2.52.6、AppIndicator、Noto CJK）；XWayland 下渲染 “Codex 工作日报 · 成员端” 1120x820 窗口，截图为真实界面；数据 0700、无启动错误；DBus 会话内单实例生效；Wayland 后端也能启动（WSLg 不支持 wlr-screencopy 故无 Wayland 截图）；Xvfb 复现截图。
- 证据 `artifacts/acceptance/ubuntu-2404-gui`。边界：非物理显示器/GNOME 面板，托盘图标未展示；HiDPI/多显示器/硬件 GPU 未验证。无代码改动，仅验证与文档。

## 0.7.0 Trae 本地明文记录来源

- 依据 0.6.2 调研：国际版 Trae 明文 `state.vscdb` 可自动读取。新增 `collector/trae_local.rs`：递归扫描 `%APPDATA%\Trae\User\workspaceStorage`（Linux 为 `$XDG_CONFIG_HOME/Trae/User/workspaceStorage`）下的 `state.vscdb`；复制库与 `-wal`/`-shm` 到临时目录后只读打开，规避 WAL 一致性；读取 `ItemTable` 中聊天存储键（`memento/icube-ai-agent-storage`、`icube-ai-*chat-storage*`、`chat.ChatSessionStore.index`），只解析含 `messages` 的会话，取 user/assistant 可见 `content`/`parsedQuery` 文本，目标日逐条过滤，无时间戳按文件日期计入并给 `说明：` 提示；加密/损坏库跳过并提示改用导出导入；文件数上限 200、单库上限 200MB、mtime 预筛。
- 成员端来源扩展为四项：Codex、DeepSeek Harness、Trae 本地记录、Trae 导出；Trae 本地记录在默认目录存在时默认启用，可关闭或自定义目录。UI 文案、同意书与 SECURITY 同步。
- 验证：成员端 22 项、无桌面核心 22 项、组长端 25 项通过（新增 5 项本地记录用例）；隔离 HTTPS `verify_trae_import` 扩展为 Codex + Trae 导出 + Trae 本地记录三来源（3 会话/6 事件，组长端校验通过）；既有回归与前端构建通过。
- 版本 0.7.0，双端与 deb 重建。真实 Trae 安装验证仍需在用户机器上补；Trae CN 仍走导出导入。

## 0.6.2 Trae 数据结构调研与解析增强

- 通过 `git` 访问 GitHub（本机 web_fetch 受限但 git 可用），调研公开 Trae 数据工具：trae-db-decrypt、trae-history、ai-data-extraction 及官方论坛帖。
- 结论：Trae 国际版聊天在 `%APPDATA%\Trae\User\workspaceStorage\<id>\state.vscdb`（明文 SQLite，ItemTable 键 `inputHistory`/`ChatStore`/`memento/icube-ai-agent-storage`）；Trae CN 在 `%APPDATA%\Trae CN\ModularData\ai-agent\database.db`（SQLCipher 4，密钥需从进程内存扫描获取，社区脚本实现；近期版本有官方“导出历史对话”）。CN 表结构：chat_session/history_v2（`messages` 为 `raw_messages` JSON），时间为 Unix 秒。
- 决策：成员端不读取 CN 加密库、不做内存扫描（越权/隐私风险），继续官方导出导入；国际版明文 vscdb 自动读取需真实样本确认 ItemTable 结构后再评估。
- 解析增强：Trae JSON 导入支持 `raw_messages`/`sessions`/`history_v2` 包装、记录级时间继承、包装标题、推理块剔除；新增 2 项回归（共 17 项）。
- 探针更新：默认复制国际版工作区 state.vscdb 与 CN database.db + WAL/SHM；全局状态（可能含令牌）默认跳过；分类区分 sqlite-plaintext/encrypted/json-text。合成目录自测通过，证据 `artifacts/acceptance/trae-probe-selftest`。
- 版本 0.6.2，双端与 deb 重建。Trae 真实样本仍需用户提供。

## 0.6.1 真实 Ubuntu 跨端验收（用户提供工作区）

- 用户提供独立 Ubuntu 24.04.4 工作区（SSH：worker@192.168.5.113:22002），本机无 sudo、无 WebKitGTK 运行时，但有 gcc/rustup 网络。
- 把 Tauri 桌面外壳改为可选 `desktop` feature（默认开启）；`--no-default-features` 在 Ubuntu 24.04 上构建采集/上传核心，避免安装 WebKitGTK。用户态解包 libssl-dev 3.0.13 头文件解决链接，未改系统。
- 真实跨端：Ubuntu 成员端二进制经 SSH 反向转发连接本机 Windows 组长端（真实 leader HTTPS + 证书指纹），配对→Codex/DSH/Trae 三来源采集→上传→状态查询全部成功；组长端 `leader.validate_capture` 通过（3 会话/8 事件，protocol 1，时区/day 合法）。证据 `artifacts/acceptance/cross-platform-real`。
- Ubuntu 24.04 上 15 项采集回归通过。
- 修复 Trae 缺时间戳提示误标“采集异常”：提示加 `说明：` 前缀，组长端分类忽略该类提示（新增 2 项单测，组长端 18 项）；真正缺口仍标记。
- 版本 0.6.1，CI 增加无桌面核心回归；Trae 真实样本仍待用户提供，Linux 桌面窗口仍未在该机验收（无 WebKitGTK/sudo）。

## 0.6 Trae 导出导入

- 需求：Trae 安装在 Windows，本地对话在加密 SQLite 库中，样本尚未提供。先把"官方导出导入"打通：成员在 Trae 中导出对话文件，选择其所在目录后与 Codex/DSH 一起采集；自动读取加密库待样本后评估。
- 采集层新增 `collector/trae.rs`：扫描导出目录（md/txt/json/jsonl，单文件上限 20MB），Markdown 按角色段落（用户/助手/系统/工具调用/工具结果，标题严格匹配避免把会话标题当角色）解析，JSON/JSONL 按消息数组/对象（role+content+时间戳）解析；系统段落与导出元数据剔除；缺时间戳按文件日期计入并告警；会话 ID 用相对路径哈希，会话标题取首个非角色标题或文件名。
- 成员端来源配置扩展为 codex/dsh/trae 三项；Trae 来源默认关闭且必须选择导出目录才能启用；会话列表/预览/来源筛选自动适配。
- 新增 6 项 Trae 解析回归与隔离 HTTPS 验收 `verify_trae_import`（Trae+Codex 合并采集上传、组长端快照校验、系统段落/元数据不出现）；既有 multi-source 与 selection 验收回归通过。
- 版本 0.6.0：双端与 Ubuntu deb 重建；提示词未改（personal.v3/extract.v2 已覆盖只有对话、无工具结果的来源）。真实 Trae 样本仍未验证。

## 0.5 Ubuntu 成员端

- 需求确认：Linux 只做成员端，组长端继续 Windows x64。
- 平台层拆分：`common.rs` 的 DPAPI/MoveFileEx 按平台实现（Linux 使用 AES-256-GCM，密钥由机器标识和登录用户派生，凭据文件 0600；原子 rename 替换）；`lib.rs` 使用 XDG 数据目录与 `xdg-open`，Linux 用单实例插件让再次启动恢复隐藏窗口，托盘构建失败只写提示不阻止启动；`main.rs` 的单实例互斥仅 Windows；`member_capture` 的选择记忆键仅 Windows 转小写；`member.rs` 设备名跨平台。
- Cargo：`windows`、`webview2-com`、`p12` 移到 Windows 目标依赖；Linux 增加 `aes-gcm` 与 `tauri-plugin-single-instance`；`build.rs` 的 manifest 链接参数仅 Windows。
- 打包：`tauri.conf.json` 增加 PNG 图标与 `linux.deb.depends`（WebKitGTK 4.1、GTK3、AppIndicator、libssl3、xdg-utils、fonts-noto-cjk）；新增 `scripts/build-linux-member.sh`；CI 增加 ubuntu-22.04 成员端任务，发布流程增加成员端 deb。
- 文档：README/INTEGRATION/SECURITY/CONTRIBUTING 增加 Linux 安装、数据目录、凭据与构建说明；UI 字体增加 Noto CJK 回退。
- 验证：WSL Ubuntu 22.04 上库回归、Release 构建、deb 打包与 Xvfb 无头启动通过；交付的 deb 用 `apt install` 实际安装成功并用已安装二进制无头启动。跨端协议分两端验证：Linux 成员端对真实 HTTPS 监听器完成配对/采集/上传/状态查询（错误指纹被拒），Linux 采集的 capture 通过真实 Windows 组长端上传校验；本机 WSL↔Windows 网络被防火墙拦截，跨端网络级实机配对留待真实环境。Windows 双端重建与既有回归不变。详见 `VALIDATION.md`。

## 0.4 多来源采集（Codex + DeepSeek Harness）

- 需求经用户确认：Linux 版本只做成员端；Trae 安装在 Windows、本地数据样本待提供；DSH 部门成员普遍使用，只读本地会话文件可接受；单条记录设上限；顺序 0.4 多来源 → 0.5 Linux 成员端 → Trae。
- 采集层重构为 `collector` 编排 + `collector/codex.rs` + `collector/dsh.rs`。Codex 逻辑与事件 ID 保持不变，新增按文件修改时间跳过不可能含当日事件的日志（本机实测采集从约 8 秒降到约 1 秒）。
- DSH 读取 `$DSH_HOME`（默认 `~/.dsh`，可自定义）下的 `sessions/--<cwd>--/<session-id>/session.vN.jsonl[.zstd]`，选择最高代际，仅解析当前 v4（Zstandard 多帧或 compression:none）。可见映射：`user/message`（仅 source.kind=user/agent-message）、`assistant/message` 的 text 块、`tool/call`、`tool/result`；排除 system/message、request/*、agent/inbox/spliced、title LLM、delivery、permission/sandbox/approval/turn/step 等内部记录。继承会话按最后带标记的 `session/end-seed` 跳过历史，缺少截点时整会话跳过并告警；旧格式 v0–v3 与较新版本跳过，仅当文件当日被修改才提示，避免历史会话长期误报；尾部半帧容错。
- 单条事件文本超过 200000 字符截断并在正文标注，采集说明同时注明上限；本机完整日志不改动。
- 成员端新增 `member.sources`、`sources.json`、`member.init.sources`；`member.codexRoot` 保留为兼容操作（仅启用 Codex）。会话选择记忆仍以 Codex 路径为键，升级后旧选择保留，新来源会话默认不选。
- 会话 ID：Codex 保持原值，DSH 使用 `dsh:<id>`；会话与事件新增 `tool`/`toolLabel`，capture 新增 `sources` 摘要。协议仍为 1，旧组长端忽略新增字段。
- UI：记录来源对话框（启用/自定义目录）、会话列表来源筛选与标签、预览显示来源、采集说明列出来源、配对同意书更新数据范围。
- 提示词：新增 `personal.v3`、`extract.v2` 说明多来源；`build.rs`、`common.rs`、`leader_runtime.rs` 更新提示词版本。`department.v2`、`merge.v1` 未改；提示词仅验证资源加密/解密与构建，本轮未调用模型。
- 验收：9 项库回归；真实本机 DSH+Codex 只读探针（6 会话、1083 事件、内部记录泄漏 0、约 1.1 秒）；隔离 HTTPS 多来源上传与组长端快照校验；选择流程回归；前端构建；Windows 双端 0.4.0 安装包重建。证据见 `VALIDATION.md` 与 `artifacts/acceptance/multi-source`。

## 0.3.6 成员补充版本冲突

- 已确认 HTTP 409 来自旧正文/来源补充提交；本机缓存显示日报来源为第2次提交、当前记录为第7次提交，未有对应新版日报。成员界面只检查定稿状态，未同步阻止旧来源提交。
- 成员端根据已有报告和接收回执明确显示等待新版，旧正文仍可阅读、草稿仍可编辑。版本冲突请求及原文保留到本机feedback-conflicts，保留较新的编辑稿；新版到达后由成员复核并重新提交。
- 独立 review 发现并修正刷新遇到450ms未保存输入、以及发送旧点击快照覆盖新编辑稿的竞态。提交绑定点击时的正文ID，刷新不会把补充附到另一版。
- 实际HTTPS与发布界面的针对性验证已通过，证据位于artifacts/acceptance/feedback-conflict。仅发布成员0.3.6，组长0.3.5保持不变；未调用模型或修改生产数据。

## 0.3.5 长记录分段完成

- 已确认真实失败提交：9会话、2683事件、约950万字符；另有同API Key并发上限5的实际错误。
- 实施顺序：①按请求Token容量规划与来源片段清单；②并行证据抽取、必要归并、统一成稿；③组长跨进程请求配额、分片进度和取消；④隔离合成场景、真实失败样例人工核对、独立review；⑤重打包组长端，成员端沿用0.3.3。
- 原始材料使用已上传快照；不在验证中修改生产报告或重新采集。失败分片可单独重试，覆盖未完整时不发布正式日报。
- 已完成 LangGraph 并行证据提取、按会话归并、跨会话状态核对、容量回查、分片缓存、跨进程请求名额和详细进度。独立 review 修正动态引用恢复、回查容量和内层 Agent 检查点继承问题，无剩余已确认阻塞。
- 聚焦验证已通过：跨进程全局峰值4、占用及排队取消、失败续跑、源范围完整性、部门补充作者/版本归属、发布界面交互；最终冻结 Agent 在外部代理不可用时仍可使用随包分词器，长记录及直接成稿路径通过。
- 真实950万字符样例分为87段。首轮受同Key其他请求占满并发影响，4段已完成后429失败；增加限流退避等待后，在相同隔离任务中复用成功片段续跑完成。
- 人工核对发现早期时区兼容问题已由后续代码修正，压缩后却保留旧风险。增加原始抽取事实与压缩基线的跨会话核对，复用原87段提取及7次归并结果，执行5批核对后重新成稿。最终共100个处理步骤；全部2683条事件的原文范围无缺口，修正后的日报已在隔离副本自动下发。
- 服务返回 Retry-After:1 时不再覆盖较长的本地退避；核对阶段限流后保留3批成功结果，最终续跑只发送2批剩余核对及1次成稿请求，67秒完成。未修改生产记录，未代替成员补充或导出本轮真实日报。
- 组长0.3.5正式安装包27,919,040字节，已更新交付目录和可运行目录；成员0.3.3安装包哈希不变。旧组长0.3.4安装包、源码和运行时保留在artifacts/legacy-0.3.4，未覆盖已安装程序。

# 进度

## 0.3.4 PDF 导出与生成取消

- 组长端个人日报、部门汇总、历史汇总仅导出 PDF；移除 Word 生成/复制路径，保留原 PDF 版式与已有文件。
- 排队或生成中可从成员行、汇总栏及任务详情取消；停止本次 Agent 进程，保留已有报告和其他任务，重新生成使用新任务。
- 原生 PDF 导出及复用、实际打包 Agent 的本地流式取消、前端取消与导出交互已通过针对性验证。仅发布组长端 0.3.4，成员端保持 0.3.3。

## 0.3.3 上传前会话筛选

- 成员端改为采集、会话筛选/完整脱敏预览、确认上传；支持搜索、项目过滤、批量勾选。同日选择本地记忆，新会话默认不选；取消不上传。
- 确认和离线重试使用同一采集快照，排除会话的内容及元数据不进入上传队列。实际 HTTPS 验证排除内容、重复确认和两端重启恢复；发布前端交互与小窗口截图已核对，现有采集聚焦回归通过，独立 review 无阻塞。
- 仅发布成员端 0.3.3，兼容现有组长端 0.3.2；未调用模型或修改生产数据。证据见 artifacts/acceptance/tauri-selection。

## 0.3.2 自动下发与直接预览

- 生成成功自动下发原本已实现，本次清除界面中的手动下发误导并明确完成状态。看板与任务完成页提供直接预览，支持版本切换、原文补充、表格和来源；任务保存具体报告 ID，预览准确版本。
- 用一次本地模型样例经过实际打包 Agent 完成自动下发、HTTPS 成员读取和补充；实际发布界面完成入口点击、版本切换及任务版本定位，截图已核对。独立 review 无阻塞，仅更新组长端。

## 0.3.1 自动识别 IP

- 修复配对默认 localhost 导致组员连接自身的问题。组长端自动检测有线/Wi-Fi 内网 IP，多网卡可选 VPN，支持手动地址；旧 localhost 设置自动迁移，邀请显示实际地址。
- 接收服务监听所有 IPv4 接口。使用真实 WLAN 10.12.39.47 完成旧配置迁移后的 TLS 配对、状态与样例上传，并核对手动主机名保留。独立 review 无阻塞；仅更新组长端，不重跑无关模型或文档测试。

## 0.3.0 Tauri 重构

- 2026-11-09 完成 Tauri 2 + TypeScript + Rust 双端迁移，组长保留 LangGraph Agent；移除安装包中的 .NET、Playwright 和 Chromium，PDF 改用系统 WebView2。
- 复用原数据格式、DPAPI、队列和归档；独立 review 修正归档匹配、内部故障重试与 Windows 时区兼容。实际 HTTPS 上传恢复、配置模型个人日报/部门汇总、Word/PDF 导出均已完成。
- 两个正式 NSIS 包已在中文空格路径安装、逐文件核对和卸载，原安装注册项未变。成员 1.76 MB，组长 26.20 MB；旧 0.2.2 包保留在 artifacts/legacy-0.2.2，新版输出至 artifacts/installers。
- 仅执行迁移相关必要验证。电脑锁屏，最终窗口输入与视觉复核尚未完成；具体证据与边界见 VALIDATION.md。

## 0.2.0 历史记录

- 2026-11-09 已核对代码与当前业务数据位置；开始0.2重构。
- 已完成组长端 LangGraph Agent、完整资料输入、Markdown 成稿、并行任务和检查点继续；成员端简化并要求实际非空补充。
- 已用真实已提交资料和实际模型跑通独立副本：3 个会话、159 条事件、223255 字符；个人约 26 秒，部门约 22 秒。Word/PDF 已导出，成员下载与归档字节一致。
- 必要回归收敛为四个业务场景并已通过；打包运行时的模拟接口检查通过。原递归摘要测试移除。
- 桌面关键操作已检查。review 发现的空终态重试、错误原文回显、Word 链接丢失、旧任务重试回显问题已修复。
- 实际文档全部页面已检查，CJK Markdown 加粗问题已修复并重新导出；未重新请求模型。
- 已完成最终 Release 构建（零警告、零错误）和四个必要业务场景回归；产物版本均为 0.2.0。
- 两端安装包已隔离安装并逐文件匹配发布目录。安装后的 Agent 完成生成、空/截断结果重试和检查点复用；内置 Chromium 实际导出中文 PDF。成员包未携带 Agent。
- 子代理确认最后的重试回显修复，无新增阻塞。安装包、源码包和校验值已输出至 artifacts/installers。

## 0.2.1 实跑问题修复

- 2026-11-09 用户提交的421事件含1513048字符，34条告警均为response_item.agent_message不支持。已识别部分为仅加密正文的内部消息、部分为可见子代理结果，需分别正常跳过和采集。
- 真实失败任务957afa1ceaaf422891b779033abf3149在流返回约890字符后失败。只读checkpoint保存原因为stream disconnected before completion，前端错误映射将其隐藏成APIError，当前缺少流中途失败后的自动重试。
- 按此实样本修复采集兼容、复用ModelRetryMiddleware处理网络/流瞬时中断，并在独立副本对指定snapshot复现；不修改正在运行的数据。
- 原始34条告警逐条经实际C#采集核对：8条可见正文正常采集、26条仅加密正文正常跳过，问题告警为0。新增格式的一个聚焦回归并入原成员闭环，四个业务场景通过。
- 同421事件原提交在artifacts/live/stream-recovery-021实际模型重跑成功：个人89秒、部门19秒，本次未再断流。Word/PDF及成员下载完成，两份PDF各2页，全部页面已检查。
- 同款显式SSE错误及静默EOF两种断流均通过框架自动重试，只有完整结果保存。review已确认生成和检查点恢复两条路径的修复，无新增阻塞。原实跑两份报告的checkpoint均确认finish_reason=stop。
- 0.2.1 Release及两端安装包已完成，零编译警告/错误。隔离安装文件与发布目录一致，安装后Agent显式断流/静默EOF恢复、完整检查点复用及Chromium导出验证完成。未覆盖正在运行的0.2.0应用或生产数据。

## 0.2.2 安装位置

- 用户要求选择安装位置。两端安装器增加目录输入/浏览，升级默认读取已注册位置，快捷方式/启动/卸载都使用所选路径；日报数据位置不变。
- 已完成两端中文空格路径的实际安装、文件比对、覆盖升级和卸载，生产注册项不变；非软件已有目录保留原文件。安装包版本均为0.2.2。
- 本轮不改报告/Agent/采集代码，复用原运行时，仅执行安装相关检查。
