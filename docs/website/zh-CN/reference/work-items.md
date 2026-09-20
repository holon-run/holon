---
title: 工作项
summary: 工作项字段、状态、生命周期操作，以及操作它们的 CLI 和 HTTP 接口。
order: 38
---

# 工作项指南

工作项是 Holon 跟踪工作时使用的持久单元。当一个目标需要自己的生命周期、
进度跟踪或跨轮次连续性时，就使用工作项。

## 什么时候使用工作项

以下情况应创建或更新工作项：

- 跨越多个轮次
- 需要可恢复的进度
- 等待外部状态，例如 CI、回调或操作者输入
- 有明确的验收标准，需要显式跟踪

以下情况不要创建工作项：

- 随口一问的问题
- 一次性的解释
- 短暂的检查
- 能立刻做完的轻量当前轮次任务

## 工作项生命周期

工作项可以是：

- **open** — 仍然活跃
- **completed** — 已显式完成
- **current** — Agent 当前的焦点
- **blocked** — 正在等待某个具体阻塞项
- **waiting for operator** — 需要澄清或批准
- **runnable** — 可以执行

关键区别在于：

- **生命周期（lifecycle）**说明目标是开放还是已完成
- **焦点（focus）**说明该项是否是当前项
- **就绪状态（readiness）**说明调度器是否应该恢复它

## 工作项存了什么

每个工作项可以包含：

- **objective** — 目标的简短陈述
- **plan artifact** — 描述预期做法的持久 markdown 文件
- **plan status** — `draft`、`ready` 或 `needs_input`
- **todo list** — 有意义的进度步骤清单
- **blocked by** — 进度无法继续时的具体阻塞项
- **recheck deadline** — 重新考虑阻塞项的回退时间

把工作项当作协调状态，而不是草稿纸。

## 核心操作

典型的工作项操作：

- `CreateWorkItem` — 创建一个新的跟踪目标
- `PickWorkItem` — 让一个 open 项成为当前焦点
- `UpdateWorkItem` — 修改目标、计划状态、阻塞项或 todo 清单
- `ListWorkItems` — 查看 current、open、blocked 或 completed 的工作
- `GetWorkItem` — 详细查看一个工作项
- `CompleteWorkItem` — 把目标标记为完成

## 工作流示例

1. 先看这个目标是否已有 open 的工作项
2. 只有目标有自己的生命周期时才创建新工作项
3. 验收边界清楚后，编辑持久计划文件
4. 有实质进展后更新 todo 清单
5. 显式记录阻塞项，而不是悄悄扩大范围
6. 只有验收证据齐备后才完成该项

## 计划状态

有意识地使用计划状态：

- **`draft`** — 目标已存在，但做法还在成形
- **`ready`** — 计划已足够稳定，可以执行
- **`needs_input`** — 下一步取决于操作者输入

这很重要，因为运行时能区分活跃可运行的工作和应该暂停的工作。

## 调度器就绪模型

工作项就绪状态是调度器的输入。open 且 runnable 的工作项可以由调度器恢复或
响应系统 tick，而 blocked 或 waiting 的项应暂停，直到解除条件发生变化。

结束一轮只会让 Agent 休息。它不会把当前工作项标记为 blocked、waiting 或
不可运行。如果暂时无法推进，就调用 `WaitFor`：

- 需要操作者输入时用 `wake=operator_input`
- 等待任务时用 `wake=task_result`，并带上 `resource=<task_id>`
- 等待外部系统时用 `wake=external`，并带上 `resource=<external object>`，
  例如 PR、CI 运行、URL 或持久 inbox 来源
- 外部系统可以主动唤醒 Agent 时，用外部触发器

只有当调度器下次恢复能取得有用进展时，才让工作项保持 runnable。这样可以避免
Agent 在休眠的同时还留着一个 open 项，被系统反复 tick。

## todo 清单的最佳实践

好的 todo 项：

- 以结果为中心
- 能跨轮次存续
- 在真实进展之后更新

避免：

- 记录每条细碎的 shell 命令
- 把 todo 清单当临时笔记
- 计划变化后仍留着过时的清单

## 阻塞与等待

工作项无法继续时：

- 调用 `WaitFor`，并给出具体的 `reason`
- 需要操作者澄清时用 `wake=operator_input`
- 具体的任务或外部等待用 `wake=task_result` 或 `wake=external`
- 只有工作确实跨轮次时，才挂上外部等待机制

这样能让调度器和后续轮次与现实保持一致。

## 与任务的关系

工作项和任务不一样：

| 面 | 用途 |
|---------|---------|
| 工作项 | 跟踪目标和进度 |
| 任务 | 表示正在运行的执行，例如命令或子 Agent |

一个工作项可能随时间创建多个任务。任务是执行；工作项是意图和进度。

## 与 Agent 的关系

Agent 可以在多个工作项之间切换焦点，但通常同一时间只维护一个当前跟踪目标。
如果目标发生了实质变化，先更新或切换工作项，再做高承诺的工作。

## 常见错误

- 每个小问题都新建一个工作项
- 范围变了却不更新计划
- 工作项一直开着却不记录阻塞项
- 没有验证证据就完成工作项

## 另见

- [运行时模型](/zh-CN/concepts/runtime-model.md) — 运行时生命周期中的工作项
- [快速示例](/zh-CN/guides/quick-examples.md) — 常见命令模式
- [多 Agent 协作](/zh-CN/guides/multi-agent.md) — 委派工作和任务监督
