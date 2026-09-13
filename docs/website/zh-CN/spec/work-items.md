---
title: 工作项
summary: 当前 WorkItem 生命周期、focus、readiness、规划、阻塞和完成契约。
order: 20
---

# 工作项

本页定义 WorkItem 运行时行为的当前契约：生命周期、焦点、就绪状态、规划、
阻塞和完成语义。

> **Last verified:** 2026-05-26 against `src/types.rs` `WorkItemRecord`,
> `WorkItemState`, `WorkItemPlanStatus`, `WorkItemReadiness`,
> `WorkItemSchedulingState`, and the tool implementations in
> `src/tool/tools/{create,update,pick,list,get,complete}_work_item.rs` and
> `src/tool/tools/wait_for.rs`.

## 源 RFC

- [工作项运行时模型](https://github.com/holon-run/holon/blob/main/docs/rfcs/work-item-runtime-model.md)
- [以工作项为中心的 Agent 运行时](https://github.com/holon-run/holon/blob/main/docs/rfcs/work-item-centered-agent-runtime.md)
- [目标、增量与验收边界](https://github.com/holon-run/holon/blob/main/docs/rfcs/objective-delta-and-acceptance-boundary.md)
- [长期上下文记忆](https://github.com/holon-run/holon/blob/main/docs/rfcs/long-lived-context-memory.md)
- [轮次内上下文压缩](https://github.com/holon-run/holon/blob/main/docs/rfcs/turn-local-context-compaction.md)

## 核心模型

工作项是 Agent 拥有的持久目标记录，跟踪以下内容：

| 字段 | 用途 |
|-------|---------|
| `objective` | 简短的人类可读目标（必填） |
| `state` | `Open`、遗留的 `Completing` 或 `Completed` |
| `plan_status` | `draft`、`ready` 或 `needs_input` |
| `plan_artifact` | agent home 中持久 plan.md 工件的路径 |
| `todo_list` | 进度清单快照 |
| `blocked_by` | 供展示的人类可读等待/阻塞描述 |
| `recheck_at` | 阻塞项重新评估的遗留回退截止时间 |
| `recheck_consumed_at` | 标记当前 recheck 提醒已投递 |
| `result_brief_id` | 已完成工作项报告的规范结果 `BriefRecord` id |
| `result_summary` | 遗留的完成摘要回退；新的完成提升不应把重复的报告文本写到这里 |

Rust 枚举 `WorkItemPlanStatus` 使用 PascalCase 变体（`Draft`、`Ready`、
`NeedsInput`），但所有工具输入/输出都使用 snake_case（`draft`、`ready`、
`needs_input`）。

## 生命周期状态

```text
                    CreateWorkItem
                          │
                          ▼
                    ┌──────────┐
                    │   Open   │
                    └────┬─────┘
                         │
            ┌────────────┼────────────┐
            ▼            ▼            ▼
       plan_status    plan_status   plan_status
        = Draft       = Ready       = NeedsInput
            │            │               │
            └────────────┼───────────────┘
                         │
                    CompleteWorkItem
                         │
                         ▼
                    ┌───────────┐
                    │ Completed │
                    └───────────┘
```

**关键契约：**

- `state` 是硬性生命周期边界：`Open` 或 `Completed`。`Completing` 是遗留的
  中间状态，新的 Agent 工具完成不再进入该状态。
- `plan_status` 是规划与协调姿态：计划仍在起草、已可执行，还是等待操作者输入。
- `plan_status=NeedsInput` 使工作项**不可运行**，意味着调度器必须等待操作者输入。
- `WaitFor` 是面向 Agent 的方式，用于把工作项标记为等待操作者输入、任务结果、
  外部资源、定时器或系统 tick。它为展示设置 `blocked_by`，并记录结构化的活跃等待。

## 就绪状态与调度

工作项就绪状态由 `state`、`plan_status`、`blocked_by`、活跃等待状态以及
延续挂起状态派生：

| `WorkItemSchedulingState` | 条件 |
|---------------------------|-----------|
| `Runnable` | open，计划不是 `NeedsInput`，无阻塞项，无活跃等待 |
| `YieldedToWorkItem` | 延续挂起：该工作项被暂停，让位给另一个工作项 |
| `WaitingOperator` | 活跃的操作者等待，或无阻塞项、无等待时 `plan_status=NeedsInput` |
| `WaitingTask` | 活跃等待任务结果 |
| `WaitingExternal` | 活跃等待外部事件 |
| `WaitingTimer` | 活跃等待定时器 |
| `WaitingSystem` | 活跃等待系统 tick |
| `Blocked` | 设置了 `blocked_by`，且没有活跃等待 |
| `Completing` | `state=Completing` |
| `Completed` | `state=Completed` |

`WorkItemReadiness` 是调度器和用户展示使用的简化视图：

| `WorkItemReadiness` | 映射来源 |
|---------------------|-----------|
| `Runnable` | `WorkItemSchedulingState::Runnable` |
| `Yielded` | `WorkItemSchedulingState::YieldedToWorkItem` |
| `WaitingForOperator` | `WorkItemSchedulingState::WaitingOperator` |
| `Blocked` | `WaitingTask`、`WaitingExternal`、`WaitingTimer`、`WaitingSystem`、`Blocked` |
| `Completing` | `WorkItemSchedulingState::Completing` |
| `Completed` | `WorkItemSchedulingState::Completed` |

**关键契约：**

- 就绪状态是派生值，不存储。`is_runnable()` 和 `is_waiting_for_operator()`
  由当前状态计算得出。
- `WaitFor(wake=operator_input)` 是操作者输入阻塞当前工作项时的显式等待信号。
- 带有 `recheck_at` 的旧阻塞工作项带有回退截止时间；调度器可在该时间之后重新
  评估它们。
- 当前焦点与就绪状态相互独立。阻塞的工作项仍可以是当前焦点，供检查使用；
  queued 和 blocked 列表过滤是基于焦点与调度器就绪状态的派生视图。

## 焦点与当前工作

一个 Agent 最多有一个**当前工作项**（`current_work_item_id`）。当前工作项是
当前轮次的焦点：

- `PickWorkItem` 从已有的 open 工作项设置当前焦点。
- `PickWorkItem(clear_blocker=true, reason=...)` 在选中该项时，还可以清除已解决
  的阻塞项和工作项范围内的等待状态。
- Agent 唤醒时，调度器可能自动选中一个可运行的工作项。
- 当前焦点跨轮次存续，直到被显式更改或完成。
- 只有可运行的工作项才有资格被调度器自动恢复。

## 工具接口

| 工具 | 用途 |
|------|---------|
| `CreateWorkItem` | 创建一个新的 open 工作项，可带计划种子和 todo_list |
| `UpdateWorkItem` | 修改 objective、plan_status、todo_list |
| `PickWorkItem` | 把当前焦点设为已有的 open 工作项；可选清除已解决的阻塞项 |
| `GetWorkItem` | 读取单个工作项，带计划预览 |
| `ListWorkItems` | 按过滤器查询：all、open、completing、completed、current、queued、yielded、blocked、waiting_for_operator、runnable |
| `CompleteWorkItem` | 按 ID 完成一个自己拥有的目标；同轮次的 assistant 文本会被提升为其完成报告 |
| `WaitFor` | 给当前工作项附加任务、外部、操作者、定时器或系统等待并让出 |

**关键契约：**

- `CreateWorkItem` 只用于真正独立、各有生命周期的目标。要细化当前工作项时，
  用 `UpdateWorkItem`，不要为同一个任务新建工作项。
- `UpdateWorkItem.todo_list` 替换整个清单快照，不是追加操作。
- 对于新的等待，`WaitFor` 取代直接修改阻塞字段。它附到当前 open 工作项上；
  如果要等待的是别的工作项，先切换过去。
- 阻塞的工作项可以被选中用于检查，但不会因此变为可运行。只有在确认阻塞项已解决
  后，才使用 `PickWorkItem(clear_blocker=true, reason=...)`；它会清除 `blocked_by`、
  回退 recheck 字段，以及工作项范围内的活跃等待。
- `CompleteWorkItem` 的文本提升：面向操作者的完成报告可以在同一 assistant 轮次中、
  调用工具之前立即写出。只调用工具的话，会返回 `awaiting_completion_report` 回执，
  并要求一次纯文本的后续轮次。
- 完成绑定在当前执行上的工作项会结算该执行并结束轮次。完成另一个自己拥有的、
  不在执行中的目标属于游离完成：当前执行、Run、焦点和轮次都保持不变。
- 控制完成端点无法完成一个带有进行中执行的 open 目标；它返回冲突，而不是清除或
  替换执行绑定。
- 运行时会把非空报告绑定到同一执行、工作项 revision、完成请求和来源工具调用，
  然后原子提交 `Open -> Completed` 转换、规范结果简报、焦点与等待清理，以及
  延续效果。
- 后续报告待处理期间，工作项保持 open，工具执行为 `Deferred`。如果轮次终止或
  报告协议被放弃，该执行会在终态事务中变为 `Interrupted`。
- 新的 Agent 工具完成不会创建遗留的 `Completing` 状态。已有的 `Completing`
  记录仍可通过控制完成 API 收尾。
- 被提升的完成报告会为该工作项恰好写一条结果简报。轮次最终结果简报被抑制，
  以免同一次完成被投递两次。
- 完成之后，其他可运行的工作项通过调度器拥有的工作队列 `SystemTick` 消息恢复，
  而不是在已完成工作项的轮次里再走一次提供商轮次。
- 计划正文的改动通过直接编辑 `plan_artifact.path` 文件完成，不经过
  `UpdateWorkItem`。

## 计划工件

每个工作项有一个可选的 `plan_artifact`，指向 Agent home 目录中的 `plan.md`
文件（`work-items/<id>/plan.md`）。计划工件：

- **不是**内联存储在工作项记录里。
- 由 `GetWorkItem` 读取（有界预览），可通过 `ApplyPatch`/文件工具编辑。
- 保存持久的文字计划；`todo_list` 是进度清单。

## 已知缺口

- `WorkItemSchedulingState` 与 `WorkItemReadiness` 有重叠；`WorkItemReadiness`
  把五种等待状态折叠为 “Blocked”，在展示层丢失了调度粒度。
- 计划工件路径解析依赖 agent home 工作区；跨 Agent 读取工作项可能需要显式的
  工作区路由。
- 包含内联 `plan` 文本的旧 ledger 快照会在工作项刷新时迁移到 AgentHome 计划
  工件。当前快照不序列化内联计划正文状态。
