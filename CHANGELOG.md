# 变更记录

## [0.7.1] - 2026-11-12

### 新增

- Trae 导出 JSON 支持**多会话拆分**：一个 JSON/JSONL 文件里包含多条会话记录时（例如社区工具解密导出的 data dump，数组元素带 `session_id`/`session_title` 与 `raw_messages`），按 session id/标题分组，不再合并成一个会话；每个会话使用自己的标题（`session_title`/`title` 或首条用户消息）与稳定且互不重复的来源 id；单会话文件行为不变。
- 支持嵌套会话包装（如 `{session_id, messages:{raw_messages:[...]}}`、`{list:[...]}`、`{sessions:[...]}`、`{conversations:[...]}`）与 JSONL 每条记录一个会话。

### 验证

- 成员端 25 项、组长端 28 项、无桌面核心 25 项库回归通过（新增 3 项：JSON dump 多会话拆分、JSONL 按 session_id 分组、无 session_id 的 list 包装）。
- 隔离 HTTPS 验收 `verify_trae_import` 扩展为 Codex + Trae Markdown + 两条 JSON dump 会话 + Trae 本地记录，共 5 会话/10 事件，组长端校验通过。
- Windows 双端与 Ubuntu 成员端 0.7.1 安装包重建（校验值见 VALIDATION）。

### 边界

- 仍无真实 Trae 导出样本；分组规则来自公开资料与常见结构，真实样本到位后复核。

## [0.7.0] - 2026-11-12

### 新增

- 成员端新增“Trae 本地记录”来源：自动读取**国际版** Trae 明文工作区 `state.vscdb`（`%APPDATA%\Trae\User\workspaceStorage`），解析 `ItemTable` 中聊天存储键（`memento/icube-ai-agent-storage` 等）的会话与可见消息，按目标日过滤；先复制数据库与 `-wal`/`-shm` 到临时目录再只读打开，避免 Trae 运行时的 WAL 一致性问题。检测到目录时默认启用，可在“记录来源”关闭。
- Trae 导出导入继续覆盖 Trae CN 与无本地明文的场景（Markdown/TXT/JSON/JSONL，含 `raw_messages` 等包装结构）。

### 变更

- 记录来源固定为四项：Codex、DeepSeek Harness、Trae 本地记录、Trae 导出；`sources.json` 自动补齐新条目。Trae 本地解析只取用户/助手可见文本，不读取 agent 工具明细或内部思考；Trae CN 加密库仍不读取、不做进程内存扫描。

### 验证

- 成员端 22 项库回归通过（新增 5 项 Trae 本地记录：明文读取、跨日过滤、加密库提示、坏载荷、凭据脱敏），组长端 25 项、无桌面核心 22 项通过。
- 隔离 HTTPS 验收 `verify_trae_import` 扩展为 Codex + Trae 导出 + Trae 本地记录三来源：3 会话、6 事件，预览仅含可见内容，组长端按生产规则校验通过，快照来源含 codex/trae/traeLocal 三类。
- 探针合成目录自测、既有 `verify_multi_source`/`verify_selection` 回归、前端 TypeScript 构建通过。
- 附：在本机 WSL2 导入的独立 Ubuntu 24.04.5 + WSLg 1.0.71 环境中，用 `apt install` 安装交付的 0.7.0 deb（自动补齐 WebKitGTK 4.1 等依赖），XWayland 下成功渲染 `Codex 工作日报 · 成员端` 窗口并截图，数据目录 0700、无启动错误，DBus 会话内单实例生效；Wayland 后端可启动。Windows 成员端用 UI Automation 验证 0.7.0 界面与“记录来源”四项来源，并走通“绑定部门→采集并选择会话→预览→上传”完整界面流程（3 会话/6 事件，组长端校验通过，状态🟢已收到）；Ubuntu 24.04 上用 xdotool 驱动同样的完整界面流程（经 SSH 反向隧道连接真实 Windows 组长端，邀请地址与 TLS 指纹端到端有效）也通过。WSLg 非物理桌面，托盘面板/HiDPI/多显示器/硬件 GPU 未验证。
- Windows 双端与 Ubuntu 成员端 0.7.0 安装包重建（校验值见 VALIDATION）。

### 边界

- 本环境没有真实 Trae 安装，本地读取规则来自公开工具的数据结构（明文 `state.vscdb`、`ItemTable` 聊天键、`messages` 列表），需在装有国际版 Trae 的电脑上复验；Trae CN 仍只支持官方导出导入。

## [0.6.2] - 2026-11-12

### 新增

- Trae 手动导出解析增强（基于公开的 Trae 数据结构调研，仍不依赖真实样本）：支持 `raw_messages`/`sessions`/`history_v2` 等包装 JSON；消息没有自身时间戳时继承记录级 `created_at`/`createdAt`；用包装的 `session_title`/`title` 作为会话标题；跳过 reasoning/thinking 等推理块。适用于社区工具解密或官方导出产生的 JSON。
- `scripts/probe-trae.ps1` 更新：默认复制国际版工作区 `state.vscdb`（聊天记录）和 Trae CN `database.db` 及 `-wal`/`-shm`；全局 `state.vscdb`/`storage.json` 可能含登录令牌，默认跳过并计数；分类增加 JSON 文本。

### 调研

- Trae 国际版聊天数据在 `%APPDATA%\Trae\User\workspaceStorage\<id>\state.vscdb`（明文 SQLite，ItemTable 键含 `inputHistory`、`ChatStore`、`memento/icube-ai-agent-storage`）；Trae CN 在 `%APPDATA%\Trae CN\ModularData\ai-agent\database.db`（SQLCipher 4，密钥需从进程内存提取，社区脚本已实现；近期版本提供“导出历史对话”）。成员端因此不读取 CN 加密库，继续走导出导入；真实样本到位后再校准解析。
- 调研参考：[trae-db-decrypt](https://github.com/Oh-My-Trae/trae-db-decrypt)、[trae-history](https://github.com/TOKIO233/trae-history)、[ai-data-extraction](https://github.com/sybil-solutions/ai-data-extraction)。

### 验证

- 成员端 17 项库回归通过（新增 2 项：`raw_messages` 记录/包装时间继承与标题、推理块剔除）。
- 探针合成目录自测通过：国际版 workspace `state.vscdb` 复制、全局状态跳过、CN 加密库与 WAL/SHM 复制、明文/加密/JSON 分类正确；证据 `artifacts/acceptance/trae-probe-selftest`。
- Windows 双端与 Ubuntu 成员端 0.6.2 安装包重建（校验值见 VALIDATION）。

## [0.6.1] - 2026-11-12

### 修复

- Trae 导出缺少逐条时间戳的提示改为 `说明：` 前缀，不再让该提交在组长端显示为“已收到／采集异常”；真正的采集缺口（旧格式会话、无法识别文件等）仍标记异常。组长端看板与成员状态使用同一分类。

### 变更

- 桌面外壳（Tauri）改为可选 `desktop` feature（默认开启）：采集与上传核心可在没有 WebKitGTK 的 Linux 环境用 `--no-default-features` 构建，便于在无桌面依赖的 Ubuntu 上验证。
- CI 的 Ubuntu 任务增加无桌面核心回归；CONTRIBUTING 补充对应命令。

### 验证

- 真实跨端验收：Ubuntu 24.04 成员端（headless core）经 SSH 隧道连接真实 Windows 组长端，完成配对→Codex/DSH/Trae 三来源采集→上传→状态查询；组长端按生产上传规则校验通过（3 会话、8 事件，来源 codex 2 / dsh 4 / trae 2），证书指纹匹配。证据：`artifacts/acceptance/cross-platform-real`。
- Ubuntu 24.04 上 15 项采集回归通过（`--no-default-features --lib`）；Windows 成员端 15 项、组长端 18 项库回归通过。
- Windows 双端与 Ubuntu 成员端 0.6.1 安装包重建；Windows 发布目录双端以隔离数据目录实际启动无错误，deb 安装/无头启动/卸载保留数据复验通过。
- 边界：该 Ubuntu 工作区无 sudo 与 WebKitGTK 运行时，Linux 桌面窗口仍在 WSL2 Ubuntu 22.04 验收；跨端经 SSH 反向转发（TLS 端到端），未测真实内网直连。Trae 真实导出样本仍待提供。

## [0.6.0] - 2026-11-12

### 新增

- 成员端新增“Trae 导出”来源：选择 Trae 手动导出的对话文件目录（Markdown/TXT/JSON/JSONL），按角色段落或消息结构解析为当日工作记录；系统段落与导出元数据不采集；缺少逐条时间戳时按文件日期计入并在采集说明中提示；上传前可在预览中核对。
- “记录来源”可分别启用 Codex、DeepSeek Harness、Trae 导出；Trae 来源必须先选择导出目录才能启用。Trae 会话在会话列表中显示来源与标题，可与其它来源一起筛选、勾选和上传。
- 新增 6 项 Trae 导入解析回归与隔离 HTTPS 验收：Trae 导出与 Codex 记录合并采集、上传并通过组长端校验。

### 变更

- 采集来源列表固定包含三个来源（未启用显示为未启用），`sources.json` 自动补齐 Trae 条目；Trae 使用手动导出文件而不是读取其加密数据库，自动解析仍待真实样本。
- 采集说明、配对同意书与 README 更新到 Trae 导出的数据范围与解析限制；提示词沿用 `personal.v3`/`extract.v2`（多来源表述已覆盖只有对话、没有工具结果的记录）。

### 验证

- 成员端 15 项、组长端 16 项库回归通过（含 Trae Markdown/JSON/JSONL、无时间戳、跨日、坏文件样例）。
- 隔离目录实际 HTTPS：Trae 导出 + Codex 合并采集与上传通过，组长端快照含两类来源且无系统段落/导出元数据；既有 Codex+DSH 上传与选择流程回归通过。
- Windows 双端与 Ubuntu 成员端 0.6.0 安装包重建；未用真实 Trae 导出样本验证，读取 Trae 本地加密数据库仍未实现。

## [0.5.0] - 2026-11-12

### 新增

- Ubuntu 22.04+ x64 成员端（`.deb`）：采集、来源筛选、上传、补充与 Windows 成员端一致；组长端仍为 Windows x64。
- Linux 凭据使用机器绑定密钥加密并保持文件 0600 权限；数据目录遵循 XDG（`~/.local/share/CodexDailyReport/Member`）。
- Linux 单实例：再次启动恢复已隐藏的窗口，GNOME 无托盘图标时同样可用；托盘不可用只记录提示，不阻止启动。
- CI 增加 Ubuntu 成员端任务，发布流程增加成员端 `.deb`。

### 变更

- 平台层拆分：Windows 保留 DPAPI、MoveFileEx、ShellExecuteW、单实例互斥与 WebView2；Linux 使用 AES-256-GCM 本机密钥、原子 rename、`xdg-open` 与单实例插件。
- `windows`、`webview2-com`、`p12` 改为 Windows 目标依赖；Linux 构建不启用组长端功能。构建脚本与源码包加入 Linux 构建入口。
- UI 字体增加 Noto CJK 回退，`deb` 依赖声明 WebKitGTK 4.1、GTK3、AppIndicator、OpenSSL、xdg-utils 与 Noto CJK 字体；Linux desktop 入口使用 `Categories=Office`，在应用菜单中归类显示。

### 验证

- WSL Ubuntu 22.04 上 `cargo test --lib`（9 项库回归）、Release 构建、`.deb` 打包与 Xvfb 无头启动通过；交付的 `.deb` 用 `apt install`/`--reinstall` 实际安装成功，已安装的二进制无头启动正常，`apt remove` 后成员数据目录保留。
- Windows 发布目录双端 0.5.0 可执行文件以隔离 `--data-dir` 实际启动，数据目录与数据库/证书创建正常、无启动错误。
- 跨端协议分两端验证：Linux 成员端对真实 HTTPS 监听器完成配对/采集/上传/状态查询（指纹正确通过、错误指纹被拒），Linux 采集的 capture 通过真实 Windows 组长端上传校验；跨端网络级实机配对受本机沙箱网络限制未执行。
- Trae 探测脚本在合成目录树上自测通过（明文 SQLite / 加密库分类与文件复制），真实 Trae 样本仍待提供。
- Windows 双端 Release 与 NSIS 安装包重建通过，成员端/组长端回归与 0.4.0 一致。
- 未在真实 Ubuntu 桌面、Wayland 会话或 ARM 设备上验收。

## [0.4.0] - 2026-11-12

### 新增

- 成员端多来源采集：同时支持本机 Codex 与 DeepSeek Harness 记录，会话列表合并展示并支持按来源、项目筛选，子代理会话标注父会话。
- 成员端“记录来源”设置：分别启用来源，可为每个来源指定自定义记录目录；旧版 `codex-root.txt` 自动迁移为 Codex 来源路径。
- 单条记录超过 200000 字符时截断，正文保留原长度和保留长度标注，采集说明同时注明；本机完整日志不受影响。

### 变更

- 个人日报与证据提取提示词升级为 `personal.v3`、`extract.v2`，说明多来源记录可能重复或记录深度不同，按实际事项合并整理。
- 采集说明、配对同意书和 README 更新为多来源范围；同日会话选择按部门、成员和时区记忆，已启用来源共用一次选择。
- 采集前按日志修改时间跳过不可能包含当日事件的文件，缩短成员端采集时间。

### 验证

- 新增 9 项库回归：Codex 结构兼容、截断、DSH v4 事件映射与内部记录过滤、继承会话截点、旧/新格式提示、损坏尾部、跨日过滤。
- 实际本机 DSH 数据只读探针通过，内部记录未进入采集内容；未上传、未写入报告。
- 隔离目录通过实际 HTTPS 多来源选择、上传与组长端快照校验；前端 TypeScript 构建与两端 Rust 库回归通过；本轮不调用外部模型。

## [0.3.6] - 2026-11-11

### 修复

- 成员端提交旧日报补充时，提前识别正文或来源已更新的版本冲突。
- 409 冲突请求和原始补充保留在本机，不再继续重试旧版本，也不自动附加到新版正文。
- 新版日报到达时保留补充草稿，并修复自动保存、刷新和点击提交之间的版本竞态。

### 构建与协作

- 增加开源仓库协作规则、版本文件、CI、发布工作流和安全说明。

## [0.3.5] - 2026-11-09

### 新增

- 长记录按 Token 容量分段，并行提取证据、按会话归并和跨会话状态核对。
- 分片进度、全局请求名额、失败续跑、取消和来源覆盖记录。

### 验证

- 对 9 个会话、2,683 条事件、约 950 万字符的隔离样例完成 87 段分析和版本化日报生成。

## [0.3.4] - 2026-11-09

- 组长端 PDF-only 导出和个人日报/部门汇总生成取消。
