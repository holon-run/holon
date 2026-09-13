---
title: 运行时模型
summary: Agent、任务、工作项、工作区，以及构成 Holon 运行时的执行循环。
order: 10
---

# 运行时模型

Holon 把 Agent 执行当作运行时系统来处理。单次模型轮次很重要，但只是一部分。
运行时把持久身份、当前工作、受监督任务、唤醒条件和最终投递当作彼此独立的
概念分别跟踪，各有自己的生命周期。

## 核心概念

### Agent

Agent 是**可寻址的运行时执行者**，包含：

- **身份** — 唯一的 `agent_id`，用于寻址消息和查看状态
- **生命周期** — 创建、活跃、休眠，最终终止
- **工作区** — 项目根目录，Agent 在其中读写文件
- **指引** — 从 `AGENTS.md` 加载（项目级和 Agent 级）
- **本地状态** — Agent 自己的记忆、待处理后续消息和工作项焦点

Agent 类型：

- **公有（自有）：** 独立身份，自管理生命周期。适合长期运行、可寻址的 Agent。
- **私有（子 Agent）：** 由父 Agent 通过任务句柄监督。适合委派的子任务和并行工作。

### 工作项

工作项是**能跨单次模型轮次存续的持久目标记录**，包含：

- **目标** — 简短的目标描述（例如“修复 src/ 中的构建告警”）
- **计划** — 用文字写下的长期多步计划
- **计划状态** — `draft`、`ready` 或 `needs_input`
- **Todo 列表** — 逐项的进度清单
- **阻塞原因** — 进度停滞时的具体阻塞描述

工作项让 Holon 能跨轮次恢复工作、查看进度，或把未完成的工作移交给其他 Agent。
工作项不是聊天历史，而是内置于运行时的**项目管理原语**。

工作项生命周期：

```
[Created] -> [Draft plan] -> [Ready] -> [In progress] -> [Completed]
                ^                            |
                +--- [Needs input] <-- [Blocked]
```

工作项完成时，运行时会提升 Agent 写下的完成文本作为**完成报告**。模式是：

1. Agent 把面向操作者的总结写成 assistant 文本
2. Agent 在同一轮次调用 `CompleteWorkItem`
3. 运行时把前面那段文本提升为规范的完成报告

完成报告保存在工作项记录里，可以通过 `GetWorkItem`、`ListWorkItems` 查看，
并由 `MemorySearch` 建立索引以便日后召回。这样就能直接问“那个 issue 最后
结论是什么”，不必翻完整份对话。

完成报告取代了自由格式的手写总结。它与工作项生命周期绑定，但不要求模型轮次
必须结束。`CompleteWorkItem` 调用就是该工作项的生命周期边界：运行时捕获对应
报告并完成该工作项。如果没有后续动作，运行时可以直接停止，不需要模型再重复
一遍同样的报告作为第二份最终简报。如果该轮次还有后续 assistant 输出、工具
调用或其他 WorkItem 完成操作，这些延续动作属于同一轮次，不会覆盖已经提升的
完成报告。

### 任务

任务是**受监督的执行句柄**，包括：

- **命令任务** — Shell 命令、构建、测试、脚本
- **子 Agent 任务** — 通过 `InvokeAgent` 调用的委派 Agent

任务生命周期独立于 Agent 面向用户的回答。你可以：

- 查看状态（`TaskStatus`）
- 读取有界输出（`TaskOutput`）
- 发送延续输入（`TaskInput`）
- 显式停止（`TaskStop`）

### 队列与唤醒

Holon 的调度原语决定 Agent 何时行动、何时休息：

- **Enqueue** — 为该 Agent 安排一条后续消息。优先级：`interject`、`next`、
  `normal`、`background`。
- **Sleep** — 没有即时工作时运行时的空闲状态。
- **Wake** — 外部触发或排队消息重新激活 Agent。

这些状态转换是可见的，集成方不需要猜测隐藏的后台行为。

### 外部触发

外部触发让 Agent 等待运行时之外的事件：

```text
Agent waits ──► External ingress provisioned ──► Event arrives ──► Agent wakes
```

Holon 会为每个 Agent 准备一个默认的外部入站能力。Agent 用它接收唤醒提示和
有内容的外部事件，不必在每个等待周期创建或取消触发器。

**投递模式：**

| 模式 | 行为 |
|------|----------|
| `wake_hint` | 唤醒 Agent，让它自行查看外部状态（例如检查 CI 运行）。提示载荷不会作为消息入队。 |
| `enqueue_message` | 唤醒 Agent，**并**把事件载荷作为消息投递到 Agent 队列。 |

当外部系统本身就有查询 API（GitHub API、CI 状态接口）时选 `wake_hint`；
当回调载荷本身就包含可操作信息时选 `enqueue_message`。

外部入站能力属于 Agent 级别。同一个 Agent 入站可以跨 PR、CI 运行、issue 和
WorkItem 复用；WorkItem 通过 `blocked_by`、`plan_status` 和 todo 状态单独记录
自己的等待状态。

### 投递

Holon 把**内部执行轨迹**和**面向用户的投递**分开：

- **简报（Brief）** — 供模型消费的压缩上下文摘要
- **最终回答** — 展示给操作者的有用结果
- **任务输出** — 命令的 stdout/stderr，可通过任务查看接口获取
- **对话记录** — 供调试使用的完整轮次历史

简报和任务结果会关联回产生它们的运行时轮次。这样 Holon 就能在唤醒和任务结果
延续之间保持连续性，而不把原始对话记录当作唯一的上下文来源。关于提示布局、
投影和压缩的用户侧模型，见[上下文连续性](/zh-CN/concepts/context-continuity.md)。

### 工作区

每个 Agent 只有一个活动工作区。工作区定义：

- **指令根** — 解析 `AGENTS.md` 和策略文件的位置
- **执行根** — 命令的默认工作目录
- **ApplyPatch 目标** — 文件变更的落点

工作区可以附加、解除附加，或隔离出来做安全试验。

## 运行循环

Agent 每次执行都遵循这个模式：

1. **入站** — 带着 `origin`、`trust`、`priority` 元数据到达。
2. **锚定** — 非平凡工作先建立一个目标稳定的工作项。
3. **加载上下文** — Agent 只读取当前决策需要的内容。
4. **变更** — 通过显式的工作区工具（`ApplyPatch`、`ExecCommand`）进行修改。
5. **验证** — 可用时运行真实项目检查（`cargo test`、`cargo check`）。
6. **投递** — 给出简洁的面向用户结果，然后休眠或安排后续消息。

## 另见

- [上下文连续性](/zh-CN/concepts/context-continuity.md) — Holon 如何在轮次之间保持模型可见的连续性
- [记忆系统](/zh-CN/concepts/memory.md) — Holon 如何在轮次之间保持连续性
- [信任边界](/zh-CN/concepts/trust-boundaries.md) — Holon 如何分类并执行信任
- [CLI 参考](/zh-CN/reference/cli.md) — 全部 CLI 命令
- [集成指南](/zh-CN/guides/integration.md) — HTTP 控制平面 API
- [快速开始](/zh-CN/getting-started/first-agent.md) — 上手教程
