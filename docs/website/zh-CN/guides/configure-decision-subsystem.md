---
title: 配置咨询决策子系统
summary: 选择决策模型路由、设置调用限额，并验证决策建议或显式弃权行为。
order: 140
---

# 配置咨询决策子系统

`AdvisoryDecision` 工具允许 Agent 在多个明确的选项之间请求非权威性的第二意见。其结果仅作为参考建议，绝不能用于直接授予系统权限或批准高风险操作。有关路由模型，请参阅[模型与服务商](/zh-CN/concepts/models-and-providers.md)。

## 前置条件

- 守护进程正在运行，且主对话模型配置正常；具体请参阅[配置模型](/zh-CN/guides/configure-models.md)。
- 具有修改共享决策配置的权限。
- 若采用本地推理，需要支持 `local-onnx` 特性的二进制版本以及已校验的模型文件。下载预设模型需要网络连接；本地推理过程不会将决策提示词发送给外部远程服务商。
- 若采用远程推理，需要具备相应凭据，以及显式声明支持 Decision 能力且提供受支持 `decision_protocol` 的模型路由。

## 第 1 步：选择决策服务商

在修改配置前，先查看当前的选择状态：

```bash
holon config get decision.enabled
holon config get decision.model
holon config get decision.local_onnx.enabled
holon config models list
```

在以下两种路径中二选一。远程 `decision.model` 与启用的本地 ONNX 无法同时生效。单纯具备常规聊天能力（chat）并不足以支撑 Decision 路由。

## 第 2 步：配置服务商

### 方案 A：使用本地 ONNX 决策服务商

在 Web GUI 中，打开 **Settings → Decision Settings**，选择 `jev-selector-q4f16` 预设，并点击 **Download and verify preset**。等待界面显示就绪状态后再启用本地推理。若使用预先准备好的模型文件，可改用 `decision.local_onnx.model_dir`；模型文件必须符合受支持的模型与清单格式。

随后通过命令行配置本地路由：

```bash
holon config unset decision.model
holon config set decision.local_onnx.preset jev-selector-q4f16
holon config set decision.local_onnx.num_threads 2
holon config set decision.local_onnx.enabled true
holon config set decision.enabled true
```

若从自定义模型目录切回受管预设，需执行 `holon config unset decision.local_onnx.model_dir`，避免覆盖预设路径。受管预设保存在 `<HOLON_HOME>/models/decision/local-onnx/<preset>/` 下。

### 方案 B：使用远程决策服务商

按照[模型配置指南](/zh-CN/guides/configure-models.md)完成服务商认证。
若使用内置的 TypeSafe Jev 路由：

```bash
holon config set decision.local_onnx.enabled false
holon config set decision.model "typesafe@default/jev-latest"
holon config set decision.enabled true
```

若接入自定义决策服务，需先配置其端点与凭据源。在 `models.catalog` 中注册实际模型，标注 `capabilities.decision: true` 及其对应的协议：`jev`、`cloudflare_clef`、`openai_decisions` 或 `openai_compatible`。编辑该对象时请保留已有条目。随后使用 `decision.model` 选中对应的 `provider@endpoint/model` 路由。

这些协议名称对应 Holon 的适配器类型；它们并不保证你的服务商账户一定开通了相应模型或端点权限。特别是 `openai_decisions` 使用基于 API Key 的 `openai_responses` 传输协议访问专用的 `/v1/decisions` 端点，而不是 OAuth Codex 协议。精确的元数据字段定义请参阅[配置参考](/zh-CN/reference/configuration.md#模型目录覆盖-modelscatalog)。

## 第 3 步：启用咨询工具并设置安全护栏

```bash
holon config set decision.tools.enabled true
holon config set decision.tools.max_calls_per_turn 3
holon config set decision.tools.min_confidence 0.65
holon config set decision.tools.timeout_ms 10000
```

未启用前，工具对 Agent 保持隐藏。上述配置用于限制单轮最大调用次数、要求最低置信度阈值，并限定工具等待超时时间。服务商层面的超时、并发与队列配置彼此独立；具体请参阅[决策子系统配置表](/zh-CN/reference/configuration.md#决策子系统配置项)。

留意标准错误输出中的配置变动提示。如果提示需要重启，请待进行中的任务收敛后，按正常流程重启守护进程再行测试。保存配置项并不代表运行中的守护进程已自动热更新生效。

## 第 4 步：通过测试提示词进行验证

使用已有的测试 Agent；将 `decision-test` 替换为你实际的 Agent ID：

```bash
holon config doctor
holon run --agent decision-test "调用一次 AdvisoryDecision 工具，传入问题'哪个审查步骤应该最先执行？'，选项列表['阅读 Diff', '发布审查结论']，状态说明'Diff 尚未经过检查；发布审查结论尚未获得授权。'汇报返回的结果与原因。不要发布任何结论，也不要修改文件。"
holon transcript --agent decision-test --limit 30
```

## 确认结果

执行轨迹中应包含实际的 `AdvisoryDecision` 工具调用及其返回结果。
核查 `outcome`、`choice`、`confidence`、`abstain`、`reason` 以及服务商/模型元数据。合法的返回结果包括选中某个选项、报告服务商回退、或显式弃权（abstain）。显式的弃权能够验证失败处理路径，但不代表推理本身成功。请如实记录弃权原因，不可将其当作有效选项处理。

如果 Agent 全程未调用该工具，说明本次测试并未真正验证 Decision 路由。决策系统的建议仅供 Agent 综合权衡，绝不能作为发布变更或修改状态的授权依据。

## 故障排查

- **工具未暴露：** 检查 `decision.enabled` 与 `decision.tools.enabled` 是否均已开启，检查当前生效配置，并核对 Agent 允许使用的工具清单。
- **路由冲突：** 使用本地 ONNX 时需清除 `decision.model`；使用远程模型时需禁用本地 ONNX。
- **预设缺失或构建版本不支持：** 检查 GUI 中的下载状态，确认当前二进制构建是否包含 `local-onnx` 支持。切勿以为单纯选择预设就会自动触发后台下载。
- **服务商不可用或拒绝请求：** 检查凭据有效性、路由能力声明、协议匹配度、端点网络连通性，以及是否已按要求重启守护进程。
- **显式弃权（Abstention）：** 检查返回原因是否由超时、服务商故障、调用次数超限、选项不合法或置信度偏低引起。切勿单纯为了强制得出建议而随意调低置信度下限。

若希望隐藏该工具同时保留服务商配置：

```bash
holon config set decision.tools.enabled false
```

## 后续阅读

- [配置参考](/zh-CN/reference/configuration.md) — 完整的配置键说明。
- [模型工具 Schema 清单](/zh-CN/reference/model-tool-schema-inventory.md) — 工具 Schema 定义。
- [Web GUI 指南](/zh-CN/guides/use-web-gui.md) — 决策设置与遥测监控。
