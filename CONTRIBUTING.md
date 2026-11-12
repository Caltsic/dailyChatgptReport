# 参与开发

感谢参与 Codex 工作日报系统。当前主线是 Windows x64 的 Tauri 2 双端软件：成员端负责采集、筛选、上传和补充，组长端负责本部门 Agent、报告版本和 PDF 输出。成员端另有 Ubuntu 22.04+ x64 构建；组长端保持 Windows-only。

## 开发边界

- 当前运行代码位于 `desktop/`、`agent/`、`prompts/` 和 `scripts/`。
- `src/`、`tests/`、`tools/` 保留旧 WPF 迁移参考；除非任务明确要求，不要把旧路径接回新版构建。
- 成员端不配置模型，也不携带组长端 Agent 或核心提示词。
- 不提交部门数据库、提交材料、配对凭据、API Key、隔离实跑结果、安装产物或本地缓存。
- 不使用真实部门数据和真实 API Key 做 CI；模型测试只使用本地 HTTP fixture。

## 分支和提交

- `main` 是可发布主线；功能开发使用短生命周期分支，例如 `feat/member-feedback-conflict`、`fix/chunk-retry`。
- 提交标题使用 `<type>: <summary>`，`type` 使用 `feat`、`fix`、`docs`、`test`、`build`、`ci` 或 `refactor`。
- 一个提交保持一个可解释的变更范围。发布前更新 `CHANGELOG.md`、`VERSION` 和受影响的组件版本。
- 发布标签使用 `vMAJOR.MINOR.PATCH`，标签必须指向已通过 CI 的提交。

## 本地检查

```powershell
cd desktop
npm.cmd ci
npm.cmd run build
cargo test --manifest-path src-tauri/Cargo.toml --lib
cd ..
python agent/pipeline_smoke_test.py
```

这些命令不调用外部模型。完整 Windows 安装包由发布工作流生成。

Ubuntu 成员端（22.04+）本地检查：

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev libssl-dev libxdo-dev \
  build-essential pkg-config
cd desktop
npm ci
npm run build
cargo test --manifest-path src-tauri/Cargo.toml --lib   # 不要加 --features leader
```

发布 `.deb` 由 `scripts/build-linux-member.sh` 或发布工作流生成。

## Pull Request

PR 说明应包含：用户可见行为、受影响的成员端/组长端、必要验证、是否改变数据格式或版本规则。涉及采集范围、提示词、报告来源和补充版本时，要附上相应的边界说明。不要把本地真实日报或 API 错误中的敏感内容复制到 issue/PR。
