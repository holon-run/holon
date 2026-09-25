---
title: 配置决策子系统
summary: 配置专用的 Decision 提供者并启用 AdvisoryDecision 工具，让 Agent 获取非权威的咨询第二意见。
order: 36
---

# 配置决策子系统

在复杂工作流中，Agent 常常需要在互斥的技术方案或执行策略间权衡。Decision 子系统为 Agent 提供了向专用模型咨询第二意见的能力（通过 `AdvisoryDecision` 工具），同时确保该建议绝不具备越权执行权限。

本指南介绍如何选择决策提供者、配置模型路由以及设定调用防护阈值。

## 前置条件

- 运行中的 Holon 守护进程（v0.45.0 或更高版本）。
- 已为 Agent 配置好主对话模型。
- 本地推理场景：包含 `local-onnx` feature 的安装（官方发布的发布包二进制已默认包含）。
- 远程推理场景：支持 Decision 能力的提供商 API 凭据或端点（例如 TypeSafe Jev 或兼容 OpenAI 协议的端点）。

## 第一步：选择决策提供者

Holon 的决策子系统支持两类架构：

1. **本地 ONNX（零网络外发）：** 完全在本地 CPU 上运行精简分类模型，数据不离开你的机器。
2. **远程提供者：** 通过 HTTP 向外部专用模型发送结构化查询（如 TypeSafe Jev 或 OpenAI 兼容端点）。

对数据隐私或离线环境有要求的场景，推荐使用本地 ONNX 提供者。

## 第二步：配置提供者

### 方案 A：使用本地 ONNX 提供者

启用本地提供者并指定预设：

```bash
holon config set decision.enabled true
holon config set decision.local_onnx.enabled true
holon config set decision.local_onnx.preset "jev-selector-q4f16"
```

如果机器有空闲 CPU 核心，可以提高推理线程数：

```bash
holon config set decision.local_onnx.num_threads 2
```

### 方案 B：使用远程提供者

直接设置决策模型的路由：

```bash
holon config set decision.enabled true
holon config set decision.model "typesafe@default/typesafe-ai/jev"
```

你也可以在 Web GUI 的 **设置** → **Decision 设置** 中直接可视化配置。

## 第三步：启用咨询工具并配置防护上限

出于安全防御考量，`AdvisoryDecision` 工具默认对 Agent 隐藏。你需要显式启用它并设置合理的防护边界，避免模型陷入无休止的反复决策：

```bash
# 向 Agent 暴露咨询工具
holon config set decision.tools.enabled true

# 限制每个对话轮次最多调用 3 次
holon config set decision.tools.max_calls_per_turn 3

# 要求至少 65% 的置信度；低于该分数时必须显式弃权（abstain）
holon config set decision.tools.min_confidence 0.65

# 设置工具超时时间（毫秒）
holon config set decision.tools.timeout_ms 10000
```

## 第四步：使用测试任务验证

运行一个包含明确选项权衡的任务：

```bash
holon run "评估 PostgreSQL 中 50 行小表查询应使用索引扫描还是全表扫描。给出最终建议前先咨询 advisory decision 工具。"
```

检查执行过程或最近记录：

```bash
holon transcript --last
```

你将在工具调用流中看到 `AdvisoryDecision`：
- `question`：具体权衡的问题。
- `options`：参与评估的候选方案。
- `outcome`：`select`（包含建议方案与置信度分数）或 `abstain`（置信度低于阈值时弃权）。

可以看到，Agent 将建议作为参考证据接收，并由 Agent 自行决定是否采纳该建议。

## 后续探索

- [配置参考](/zh-CN/reference/configuration.md)：全部 `decision.*` 配置项说明。
- [模型工具 schema 清单](/zh-CN/reference/model-tool-schema-inventory.md)：`AdvisoryDecision` 的机器可读输入与输出契约。
- [Web GUI 指南](/zh-CN/guides/use-web-gui.md)：在浏览器中管理决策设置与查看遥测指标。
