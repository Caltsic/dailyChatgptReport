# 0.7.0 附：Ubuntu 24.04 桌面窗口

- [x] 本机 WSL2 导入独立 Ubuntu 24.04.5 发行版（cloud-image rootfs），WSLg 1.0.71。
- [x] 交付 deb 用 apt 安装，XWayland 下渲染成员窗口并截图（`artifacts/acceptance/ubuntu-2404-gui`），数据 0700、无启动错误、单实例生效、Wayland 后端可启动、Xvfb 复现。
- [x] VALIDATION/README/progress 更新。
- [ ] 物理 Ubuntu 桌面/GNOME 面板、HiDPI、多显示器、硬件 GPU、真实 Wayland 会话截图仍待验证。
- [ ] Trae 真实安装与样本仍待用户机器。

# 0.7.0 Trae 本地明文记录

在 0.6.2 调研基础上，实现国际版 Trae 明文工作区记录的自动采集（不依赖样本，但需真实机器复验）。

## 阶段
- [x] `collector/trae_local.rs`：扫描 workspaceStorage 下 `state.vscdb`，临时复制库+WAL/SHM 后只读解析 ItemTable 聊天键，目标日过滤、脱敏/截断、加密与坏库提示。
- [x] 成员端来源扩展为四项，默认检测到国际版 Trae 目录时启用；UI/同意书/文档/SECURITY 更新。
- [x] 回归：成员端 22 项、组长端 25 项、无桌面核心 22 项；隔离 HTTPS 验收覆盖 Codex + Trae 导出 + Trae 本地记录三来源；既有验收与前端构建通过。
- [x] 版本 0.7.0 与安装包重建。
- [ ] 在装有国际版 Trae 的真实电脑上复验本地读取（本轮只有合成 `state.vscdb`）。
- [ ] Trae 真实导出样本核对；Trae CN 仍只支持导出导入。

# 0.6.2 Trae 数据结构调研与解析增强

用户 2026-11-12 表示 Trae 先保留疑问、后续补样本。本轮用公开资料把不依赖样本的部分做扎实。

## 阶段
- [x] 调研公开 Trae 工具（trae-db-decrypt/trae-history/ai-data-extraction）：国际版明文 vscdb 路径与键名；CN SQLCipher 库路径、表结构与密钥获取方式；官方导出能力。
- [x] 解析增强：Trae JSON 导入支持 raw_messages/sessions/history_v2 包装、记录级时间继承、包装标题、推理块剔除（17 项回归）。
- [x] 探针更新：默认复制国际版工作区 state.vscdb、CN database.db+WAL/SHM；全局状态默认跳过；分类更细；合成目录自测。
- [x] 版本 0.6.2、文档与安装包重建。
- [ ] 真实 Trae 导出样本核对；国际版明文 vscdb 自动读取评估（需样本确认 ItemTable JSON 结构）。
- [ ] CN 加密库解密不实现（隐私/越权风险），只支持官方导出。

# 0.6.1 真实 Ubuntu 跨端验收

依据 2026-11-12 用户提供独立 Linux 工作区（worker@192.168.5.113:22002），补齐 0.5 未完成的实机跨端验证。

## 阶段
- [x] 桌面外壳改为可选 `desktop` feature；无桌面核心可在 Ubuntu 24.04 构建（用户态 libssl-dev 解包，无需 sudo）。
- [x] Ubuntu 24.04 采集回归 15 项通过；真实成员端二进制经 SSH 反向转发连接 Windows 组长端完成配对/三来源采集/上传/状态查询，组长端校验通过。
- [x] 提示分类修复：`说明：` 提示不计入“采集异常”；新增组长端分类单测（18 项）。
- [x] 文档/CI/版本更新与安装包重建（0.6.1）。
- [ ] Trae 真实导出样本核对与自动解析（待用户提供）。
- [ ] Linux 桌面窗口在带 WebKitGTK 的真实 Ubuntu 上验收（本次工作区无 sudo/运行时，仍用 WSL2 22.04 无头验收）。

# 0.6 Trae 导出导入

依据 2026-11-12 用户确认：Trae 装在 Windows、样本待提供。先实现"官方导出导入"通道，自动读取加密数据库待样本后评估。

## 阶段
- [x] 成员端 Trae 导出目录来源（Markdown/TXT/JSON/JSONL 解析、系统段落与元数据剔除、无时间戳回退）。
- [x] 来源配置与 UI/同意书更新（三项来源，Trae 需选目录才能启用）。
- [x] 解析回归、隔离 HTTPS 验收、既有 multi-source/selection 回归、双端与 deb 重建。
- [ ] 真实 Trae 导出样本核对与规则修正（待用户提供）。
- [ ] Trae 本地加密数据库自动读取（待样本评估）。

# 0.5 Ubuntu 成员端

依据 2026-11-12 用户确认：Linux 只做成员端，组长端继续 Windows。

## 阶段
- [x] 平台层拆分（凭据、原子写入、数据目录、打开文件、单实例、托盘降级、设备名）。
- [x] Cargo 目标依赖拆分与 Linux 依赖。
- [x] deb 打包配置、Linux 构建脚本、CI 与发布流程。
- [x] WSL Ubuntu 22.04 构建/回归/无头启动验证与 Windows 双端重建。

# 0.4 多来源采集（Codex + DeepSeek Harness）

依据 2026-11-12 用户确认：Linux 版本只做成员端；Trae 安装在 Windows、样本待提供；DSH 部门成员普遍使用；单条记录设上限；顺序为 0.4 多来源、0.5 Linux 成员端、Trae 接入。

## 阶段
- [x] 采集层重构为来源编排，Codex 既有行为回归。
- [x] DeepSeek Harness v4 读取、内部记录过滤、继承截点、容错与截断。
- [x] 成员端来源配置/UI/同意书，提示词与组长端版本更新。
- [x] 单元回归、真实本机数据只读探针、隔离 HTTPS 多来源验收、安装包构建。
- [ ] 0.5 Ubuntu 成员端。
- [ ] Trae 接入（探测 + 官方导出导入，待样本）。

# 0.2 LangGraph 生成内核重构

依据 2026-11-09 用户指令：成员端采集上传、复核与非空补充；组长端唯一 Agent；自然文本报告和格式示例；不用递归摘要、费用或人为轮数限制。独立报告并行，部门汇总输入全部已反馈报告。保持原业务数据可读。

## 阶段
- [x] 核对现有实现及新需求。
- [x] LangGraph 本地运行时与个人/部门提示词。
- [x] C# 组长内核接入、正文与文档兼容、并行执行。
- [x] 成员简化、非空反馈、反馈后导出与部门汇总。
- [x] 仅保留必要回归，实际长记录生成与 Word/PDF 完整路径验证。
- [x] 子代理 review、修复、安装包与交付。

## 决定
- Agent 仅部署组长端。Python LangChain create_agent（基于 LangGraph），本地子进程通过 stdin/stdout JSON 行通讯；包内带运行时，无需用户安装 Python。
- 完整已上传记录直接输入个人 Agent；全部已反馈日报和补充一起输入部门 Agent。移除自制分段递归与强制 JSON schema。
- 正文保存 Markdown 文本；旧结构化报告兼容读取。程序负责既定 Word/PDF 样式和补充原文附尾。
- 用户授权本次必要真实链路验证，使用已配置模型和已提交记录，不主动采集其他工作记录，不修改正在运行的业务数据库。

## 问题
- 无 Git 仓库；改动前保存源码恢复包。
