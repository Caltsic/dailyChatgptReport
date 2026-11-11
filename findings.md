# 发现

- 旧内核 GenerationWorker 用 18,000 字符预算递归 Group/Merge，造成23分段到2摘要空转；全部替换。
- 旧 ModelValidation 强制五数组、evidenceIds、工具结果才能完成/验证；本次改为提示词职责，输出自然成稿。
- DepartmentService.SummaryInput 只选定稿；新规则改选当前已反馈日报，内容签名包含正文和补充以判断过期。
- Member MainWindow 把 AnalysisProgress 返回到成员；移除该回显，保留上传和报告基本状态。
- Documents 当前 A4模板接受分节纯文本，需支持新 Markdown 正文和表格；补充保持原文。
- 新 Agent 实际长资料运行成功，无递归收敛阶段。中断继续以 LangGraph 已保存节点为单位，未完成的模型调用需重发，不宣称流式中间文字已持久化。
- Markdown 中文标点附近加粗需 UseCjkFriendlyEmphasis；Word 链接需显式保留目标，否则链接标注被保留但地址丢失。
- 同 ID 重试保持原 CreatedAt；看板应优先运行中任务，并使用已有 Progress.UpdatedAt 选取最近任务，避免隐藏重试及新失败。
