---
title: Agent 状态
summary: 当前的 Agent 状态、生命周期标签、运行时投影，以及面向用户的展示契约。
order: 10
---

# Agent 状态

本页定义 Holon 中 Agent 状态、生命周期和运行时投影的当前契约。契约基于下方
最后复核日期所对应的实现和测试进行验证。

> **Last verified:** 2026-07-24 against `src/types.rs` `AgentState`,
> `AgentStatus`, `AgentIdentityView`, `AgentSchedulingPosture`,
> `AgentPostureProjection`, `ClosureDecision`, `ContinuationResolution`,
> `RuntimePosture`, and `AgentSummary`; `src/storage/mod.rs`
> `agent_posture_projection`; `src/runtime/lifecycle.rs` `agent_summary`;
> `src/runtime/closure.rs` closure derivation; and `src/tool/tools/get_agent.rs`.

## 源 RFC

- [Agent 状态模型与运行时投影](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-state-model.md)
- [Agent 生命周期控制姿态](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-lifecycle-control-posture.md)
- [Agent 控制平面模型](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-control-plane-model.md)
- [Agent Profile 模型](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-profile-model.md)
- [Agent 初始化与模板](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-initialization-and-template.md)
- [Agent 删除生命周期](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-deletion-lifecycle.md)

## 权威记录与投影

Agent 状态**派生**自权威运行时记录，而不是存成单个不透明的状态字段。关键区别是：

| 层 | 内容 | 权威来源 |
|-------|------|-----------|
| **身份** | `agent_id`、kind、profile preset 和监督元数据 | Agent 注册表记录 |
| **身份生命周期** | `AgentRegistryStatus` — Active、Deleting、Deleted | Agent 身份仓库 |
| **生命周期状态** | `AgentStatus` — Booting、AwakeIdle、AwakeRunning、AwaitingTask、Asleep、Stopped | 调度器执行器（唯一写入方） |
| **调度姿态** | `AgentSchedulingPosture` — 由队列、WorkItem、任务和等待状态派生 | 调度器 `derive_posture` 投影 |
| **运行时姿态** | `RuntimePosture` — Awake 或 Sleeping | 轮次结束时的 closure 决策 |
| **延续** | `ContinuationResolution` — Agent 如何被重新激活 | 轮次开始时的 ingress/dispatch |
| **面向用户的摘要** | `AgentSummary` — 供 API/UI/模型展示的稳定投影 | `GetAgent` 工具 + HTTP `/agents/{agent_id}` |

`AgentSummary` 是**展示投影**，不是调度决策的事实来源。调度器必须从队列、
WorkItem、任务和等待状态派生姿态，而不能读取摘要字段。

当前实现锚点：

- `RuntimeHandle::agent_summary` 在读取时，从当前 `AgentState`、身份视图、
  模型状态、执行快照、活跃等待、子 Agent、外部触发器和
  `AppStorage::agent_posture_projection` 组装出 `AgentSummary`。
- `GetAgent` 直接返回组装好的 `AgentSummary`；它不写入生命周期、调度或等待状态。
- `/agents/list` 和 `/agents/{agent_id}` 走同一条运行时摘要/列表投影路径。
  `AgentListEntry` 是紧凑的列表投影，不是调度器输入。
- `AppStorage::agent_posture_projection` 从持久化/运行时记录派生出对外暴露的
  `AgentSchedulingPosture`。任何对调度敏感的路径都不应把
  `AgentSummary.scheduling_posture` 读回来当作权威。

## Agent 生命周期状态（`AgentStatus`）

```text
                ┌─────────────┐
                │   Booting   │
                └──────┬──────┘
                       │ daemon_start / Start
                       ▼
                ┌─────────────┐
         ┌─────►│  AwakeIdle  │◄─────────────┐
         │      └──────┬──────┘              │
         │             │ turn starts         │
         │             ▼                     │
         │      ┌──────────────┐             │
         │      │ AwakeRunning │             │
         │      └──────┬───────┘             │
         │             │ turn closure        │
         │             ▼                     │
         │      ┌─────────────┐     ┌────────┴──────┐
         ├──────│    Asleep    │────►│  AwaitingTask │
         │      └─────────────┘     └────────┬──────┘
         │         wake / resume              │ task result
         │                                    │
         └────────────────────────────────────┘

                          Stop ──► ┌──────────┐
                                   │  Stopped  │
                                   └──────────┘
```

| 状态 | 含义 |
|--------|---------|
| `Booting` | Agent 正在初始化；尚未交给调度器 |
| `AwakeIdle` | Agent 已唤醒，但没有正在进行的模型轮次 |
| `AwakeRunning` | 当前正在执行模型轮次 |
| `AwaitingTask` | 已唤醒的 Agent 阻塞在非终态任务结果上时使用的过渡标签 |
| `Asleep` | 运行时接受了轮次 closure，且没有模型轮次在运行 |
| `Stopped` | Agent 生命周期已停止；调度器不会启动新轮次 |

**关键契约：**

- `Asleep` 是运行时姿态，只在调度器于轮次 closure 后接受休息时到达。
  它不是权威的“空闲”声明。
- `WaitFor` 在让出前记录显式等待状态；当 WorkItem 或 Agent 等待任务、外部
  或操作者输入时，它是首选路径。
- `Asleep` 的 Agent 可以有 runnable 的 WorkItem。`Asleep` **不**表示空闲或无事可做。
- `AwaitingTask` 是当前运行时、TUI、daemon 和等待投影使用的过渡生命周期标签，
  用于非终态任务（命令、子 Agent）阻塞后续模型重入的情形。它日后可能并入
  `AwakeIdle` 加任务等待调度姿态，但在迁移发生前仍是当前契约。
- `Stopped` 是硬生命周期边界。调度器不会为已停止的 Agent 启动新轮次。
  已停止的 Agent 会释放运行时拥有的执行资源。
- 状态转换经由调度器拥有的辅助函数；任何模块都不应绕过调度器执行器直接修改
  `AgentState.status`。

## 调度姿态（`AgentSchedulingPosture`）

调度器从当前状态派生出调度姿态。这是**投影**，不是存储状态。当前归约到
Agent 级的投影按以下优先级判定：

| 姿态 | 条件 |
|---------|-----------|
| `Stopped` | Agent 生命周期已停止（`AgentStatus::Stopped`） |
| `ActiveTurn` | `AgentState.current_run_id` 已设置 |
| `HasQueuedInput` | 队列中该 Agent 有等待处理的入队项 |
| `HasRunnableWork` | 当前或排队的 WorkItem 可运行 |
| `WaitingForTask` | 某个 WorkItem 有活跃的任务等待条件 |
| `WaitingForExternal` | 某个 WorkItem 有活跃的外部等待意图 |
| `WaitingForOperator` | WorkItem 的 `plan_status=needs_input`，或有活跃的操作者等待 |
| `Blocked` | WorkItem 设置了 `blocked_by`，或有活跃的定时器/系统/非操作者等待 |
| `Idle` | 无入队输入、无可运行工作、无阻塞条件 |
| `Unknown` | 首次投影前的默认值；不属于稳定契约 |

**关键契约：**

- 姿态派生自队列深度、WorkItem 就绪状态、等待状态、任务阻塞状态和外部触发器。
- 姿态是快照派生值，不作为持久状态保存。
- 已停止的生命周期先于瞬时的运行、队列、工作或等待事实，赢得对外投影（`Stopped`）。
- 队列和可运行工作优先于被动休眠姿态。有入队输入或有可运行 WorkItem 的
  `Asleep` Agent 会投影为 `HasQueuedInput` 或 `HasRunnableWork`，而不是 `Idle`。
- WorkItem 级的 `WaitingTimer` 和 `WaitingSystem` 仍是不同的调度器等待状态，
  但归约后的 Agent 级姿态目前把它们报告为 `Blocked`；调度器的空闲边界决策仍会
  检查 WorkItem 等待状态，以发出定时器或系统 tick 动作。
- `AgentSummary.scheduling_posture` 暴露这一投影；使用者不应把它当作权威调度输入。

## Closure 与延续

每轮结束时，closure 决策决定下一个姿态：

| `ClosureOutcome` | 效果 |
|------------------|--------|
| `Completed` | 工作完成；Agent 可接收下一项工作 |
| `Continuable` | 工作继续；同一 WorkItem 保持活跃 |
| `Failed` | 轮次失败；Agent 可恢复或上报 |
| `Waiting` | Agent 正在等待操作者、外部、任务或定时器 |

等待中的 Agent 被重新激活时，`ContinuationResolution` 记录：

| 字段 | 含义 |
|-------|---------|
| `trigger_kind` | OperatorInput、TaskResult、ExternalEvent、TimerFire、InternalFollowup、SystemTick |
| `class` | ResumeExpectedWait、ResumeOverride、LocalContinuation、TaskResultReentry、LivenessOnly |
| `model_reentry` | 模型是否应带着上下文重新进入 |
| `matched_waiting_reason` | 触发器是否与先前的等待原因匹配 |

Closure 推导与展示姿态相互独立。它使用调度器投影事实和当前轮次事实来选择
`ClosureOutcome`、`WaitingReason` 和 `RuntimePosture`。当前实现中：

- 显式操作者等待在 closure 原因上优先于其他等待条件；
- 阻塞任务只是元数据，除非有当前工作等待表示它；
- 活跃的工作项或 Agent 等待意图映射为外部等待；
- 定时器映射为定时器等待；
- 可运行工作可以阻止无关的 Agent 级等待意图成为 closure 原因。

## 面向用户的投影（`AgentSummary`）

`AgentSummary` 是由 `GetAgent` 和 `GET /api/agents/{agent_id}` 返回的稳定投影。
它包含：

- `identity` — Agent 身份徽标和 profile
- `agent` — 核心 `AgentState`，含状态、待处理计数、轮次索引
- `scheduling_posture` — 派生姿态快照
- `lifecycle` — 生命周期提示（非权威）
- `model` — 当前模型选择
- `token_usage` — token 用量摘要
- `closure` — 最近一次 closure 决策
- `execution` — 执行快照（run id、cwd、workspace）
- `active_children` — 可见的子 Agent 摘要
- `active_wait_conditions` — 当前等待状态
- `active_external_triggers` — 已配置的外部 ingress 能力

**关键契约：**

- `AgentSummary` 在读取时从权威记录组装。
- 新增字段保持保守；不要把 `AgentSummary` 当作内部状态的堆放处。
- 模型通过 `GetAgent` 收到 `AgentSummary` 作为展示信息，而不是调度指令。
- API 使用者不得依赖摘要字段顺序或默认/空字段是否出现。

### v0.38.0 身份迁移遗留

`AgentVisibility`、`AgentOwnership`、`PrivateChild` 和 `PublicNamed` 仅描述
v0.38.0 的迁移面。它们不是当前的公开身份契约，不得用于选择创建或委派行为。
当前调用方使用 `CreateAgent` 创建可寻址 Agent，使用 `InvokeAgent` 做父级监督的
委派执行。

## 生命周期控制

Agent 执行生命周期控制是 `Start` / `Stop`：

- `Start` 把 Agent 交给调度器。它**不**直接启动模型轮次；由调度器决定 Agent
  应保持空闲还是处理入队输入。
- `Stop` 中止当前运行，释放运行时拥有的执行资源，并把 Agent 标记为不可运行。
  入队消息和持久记录会被保留。
- 没有 `Pause` / `Resume`。契约就是 `Stop` + `Start`。

## 验证结论

本页对照 issue #1367 中列出的 RFC 和实现区域进行了验证。

| 领域 | 结论 | 分类 | 当前处理 |
|------|---------|----------------|------------------|
| `AgentSummary` / `GetAgent` 推导 | 摘要在读取时组装；`GetAgent` 只读，不修改状态。 | 契约与实现一致 | 上文已记录。 |
| 投影作为调度器输入 | 面向用户的 `AgentSummary.scheduling_posture` 派生自存储/运行时事实。对调度敏感的 closure 和 run-loop 路径从队列、WorkItem、等待、任务和轮次状态派生，而不是读回摘要。 | 契约与实现一致 | 由存储和运行时测试覆盖。 |
| 生命周期标签 | 当前实现保留了 `Booting`、`AwakeIdle`、`AwakeRunning`、`AwaitingTask`、`Asleep` 和 `Stopped`；`Paused` 仅作为 `Stopped` 的遗留别名反序列化。 | 契约与实现一致，含过渡标签 | 记录为当前契约和已知迁移缺口。 |
| Agent 级定时器/系统等待 | WorkItem 调度区分 `WaitingTimer` 和 `WaitingSystem`，而归约后的 `AgentSchedulingPosture` 把它们报告为 `Blocked`。 | 有意为之的归约投影 | 已记录；测试覆盖该投影。 |
| `Stopped` 姿态 | `AgentSchedulingPosture::Stopped` 覆盖已停止的 Agent；`archived` 仅保留为 serde 别名。 | 旧规范措辞已过时 | 已在本页修正。 |
| 持久状态与运行时投影 | `AgentState`、队列项、WorkItem、任务、等待条件/意图、外部触发器和审计/转录记录仍是权威；`AgentSummary` 仍是展示/API 投影。 | 契约与实现一致 | 记录为分层表和锚点。 |

## 已知缺口

- `AgentStatus` 仍包含过渡状态（`Booting`、`AwaitingTask`），随着调度器模型成熟
  它们可能被合并。若 `AwaitingTask` 最终完全并入 `AwakeIdle` + 任务阻塞，见后续跟进。
- `AgentLifecycleHint` 携带生命周期投递提示，例如是否接受外部消息，以及可选的
  操作者指引。已废弃的 Pause/Resume 投影字段不属于契约。
- `AgentSummary` 含一些契约尚未固化的字段（`recent_operator_notifications`、
  `recent_brief_count`）。
- `AgentStatus::Asleep` 仍是生命周期/展示投影，但调度器的空闲边界决策会先检查
  等待和工作事实，再把已休眠的 Agent 当作空闲。
- `AgentStatus::AwaitingTask` 在代码中仍是过渡状态，尽管它不在长期目标状态集
  （`agent-lifecycle-control-posture.md`）中。
- `AgentSchedulingPosture::Stopped` 覆盖已停止的 Agent；`archived` 只作为 serde
  别名保留，与之前“当前未使用”的规范说法相反。
