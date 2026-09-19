---
title: 调度器
summary: 当前调度器输入、runnable/waiting 决策、WorkItem readiness 和 wake/sleep 边界。
order: 30
---

# 调度器

本页定义 Holon 调度器的当前契约：它消费哪些输入、如何推导调度姿态与可运行性、
以及发出哪些决策。页面同时记录附加的协议转换层：它把调度器决策包进带重放保护的
原子事务，并承担显式 activation 归属、终态结算和公开诊断事件流。

> **Last verified:** 2026-09-13 against `src/runtime/scheduler.rs`,
> `src/runtime/scheduler_executor.rs`, `src/runtime/waiting.rs`,
> `src/runtime/closure.rs`, `src/runtime/turn/execution.rs`,
> `src/runtime_db/transitions.rs`, `src/runtime_event.rs`, and `src/types.rs`.

## 源 RFC

- [运行时调度器契约](https://github.com/holon-run/holon/blob/main/docs/rfcs/runtime-scheduler-contract.md)
- [调度器与 WorkItem 统一执行协议](https://github.com/holon-run/holon/blob/main/docs/rfcs/scheduler-work-item-unified-execution-protocol.md)
- [调度器切换简化](https://github.com/holon-run/holon/blob/main/docs/rfcs/scheduler-cutover-simplification.md)
- [调度器等待状态与可恢复的 Agent 延续](https://github.com/holon-run/holon/blob/main/docs/rfcs/scheduler-wait-state.md)
- [等待平面与重新激活](https://github.com/holon-run/holon/blob/main/docs/rfcs/waiting-plane-and-reactivation.md)
- [延续触发器](https://github.com/holon-run/holon/blob/main/docs/rfcs/continuation-trigger.md)
- [以 WorkItem 为中心的 Agent 运行时](https://github.com/holon-run/holon/blob/main/docs/rfcs/work-item-centered-agent-runtime.md)
- [Agent 激活、结算与分发](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-activation-settlement-and-dispatch.md)：历史性的激活/结算设计；在统一协议分配了当前权威之处已被取代

## 核心模型

调度器回答的问题是：给定 Agent 的当前状态，下一步该做什么？

它消费一个 `SchedulerProjection`，即由下列来源汇总而成的快照：

| 输入 | 来源 |
|-------|------|
| Agent 状态 | `AgentState.status` |
| 队列深度 | `AgentState.pending` |
| 活跃任务 | 状态非终态的 `TaskRecord` |
| 当前 WorkItem | `current_work_item_id` → `WorkItemRecord` |
| 可运行 WorkItem | `is_runnable()=true` 的开放 WorkItem |
| 等待条件 | 活跃的 `WaitConditionRecord` |
| 等待意图 | 活跃的 `WaitConditionRecord`，按 Agent 和 WorkItem 范围划分 |
| 唤醒提示 | `PendingWakeHint` |
| 轮次状态 | `turn_in_progress`、`last_turn_terminal` |
| 运行时错误 | `runtime_error_active()` |

投影是**只读快照**；调度器从不直接修改持久状态。决策发出后交给执行器。

## 调度器输入（`SchedulerInput`）

| 输入变体 | 触发条件 |
|---------------|---------|
| `Message` | Agent 队列中到达了一条新消息 |
| `IdleSignal::WakeHint` | 收到一个待处理的唤醒提示 |
| `IdleSignal::ContinueActive` | 上一次轮次结束时有可运行的 WorkItem |
| `IdleSignal::QueuedAvailable` | 队列中的消息已可处理 |
| `Idle` | 周期性的空闲边界检查 |

## 调度器决策（`SchedulerDecisionKind`）

| 决策 | 含义 |
|----------|---------|
| `StartModelTurn` | 组装上下文并开始一次新的模型轮次 |
| `ReduceMessageOnly` | 只归约一条消息，不启动完整模型轮次 |
| `EmitSystemTick` | 发出运行时自有的后续消息（system tick） |
| `WaitForTask` | 阻塞，直到某个非终态任务完成 |
| `WaitForExternalChange` | 阻塞，直到外部事件到达 |
| `WaitForTimer` | 阻塞，直到定时器触发 |
| `WaitForOperator` | 阻塞，直到操作者输入到达 |
| `Sleep` | 运行时让 Agent 进入休眠；没有立即动作 |
| `StayIdle` | Agent 已处于休眠；没有动作 |
| `Stop` | Agent 已停止；无法调度 |
| `Noop` | 没有动作（重复被抑制，或轮次进行中） |

每个决策都携带元数据：`reason`、`model_reentry`、`liveness_only`、
`work_item_id`、`task_id` 和 `evidence`。

## 决策流程

```text
                    SchedulerInput
                         │
                         ▼
              ┌─────────────────────┐
              │ Status == Stopped?  │──Yes──► Stop
              └─────────┬───────────┘
                        │ No
                        ▼
              ┌─────────────────────┐
              │ Turn in progress?   │──Yes──► Noop
              └─────────┬───────────┘
                        │ No
                        ▼
         ┌──────────────────────────┐
         │ Queue has pending input? │──Yes──► StartModelTurn
         └──────────────┬───────────┘        (or ReduceMessageOnly)
                        │ No
                        ▼
         ┌──────────────────────────┐
         │ Runnable WorkItem?       │──Yes──► EmitSystemTick
         └──────────────┬───────────┘        (ContinueActive)
                        │ No
                        ▼
         ┌──────────────────────────┐
         │ Active wait condition?   │──Yes──► WaitFor{Task,
         └──────────────┬───────────┘         External,Timer,Operator}
                        │ No
                        ▼
                      Sleep
```

## WorkItem 调度状态

WorkItem 会经过调度器消费的这些调度状态：

| 状态 | 含义 | 调度器动作 |
|-------|---------|-----------------|
| `Runnable` | 可以处理 | 可能被自动选为当前 WorkItem |
| `WaitingOperator` | `plan_status=NeedsInput` 或操作者等待 | Agent 等待操作者 |
| `Blocked` | 设置了 `blocked_by` 但没有更具体的等待 | 不可运行；存在遗留 `recheck_at` 时检查它 |
| `WaitingTask` | 对任务结果的等待条件 | 任务进入终态时唤醒 |
| `WaitingExternal` | 对外部事件的等待条件 | 外部触发时唤醒 |
| `WaitingTimer` | 运行时定时器等待 | 定时器触发时唤醒 |
| `WaitingSystem` | 运行时 system-tick 等待 | 发出 system tick |
| `Completed` | `state=Completed` | 从可运行集合中排除 |

## 唤醒/休眠边界

- `Sleep` 是轮次结束后的内部调度器决策。调度器据此决定 Agent 真正进入
  `Asleep`，还是带着队列中的工作继续。
- `WaitFor` 记录显式等待状态，然后让出轮次。它是面向模型的任务、外部和操作者
  等待路径。
- `StayIdle` 表示 Agent 已处于休眠，调度器无事可做；它与 `Sleep`（首次转换）
  不同。
- `EmitSystemTick` 注入一条内部后续消息，让模型在空闲边界发现可运行 WorkItem
  时重新进入。
- `CompleteWorkItem` 报告晋升结束轮次后，其余可运行的 WorkItem 由同一条工作队列
  `SystemTick` 路径恢复。
- 唤醒提示是**活性信号**：它们让调度器重新评估，但本身不携带面向模型的内容。
- 重复抑制使用幂等键，避免对同一个唤醒提示或 continue-active 信号产生多余的
  system tick。

## 协议转换层

调度器把每个边界都包进原子的 `QueueTransitionCommand` 事务，该事务可以同时：

1. 提交队列操作（admit、claim 或 enqueue）；
2. 更新 Agent 状态投影；
3. 持久化消息证据、transcript 条目和审计事件；
4. 绑定规范的 activation 归属方和执行处置；
5. 持久化结算、恢复和投递证据。

所有效果在同一个 SQLite 事务中提交。事务失败或 CAS 不匹配时，不会留下任何部分的
队列、activation、结算或投递状态。

规范调度器是唯一的运行时引擎。队列、WorkItem、等待、任务、Turn、transcript、
brief、投递、activation、结算和执行事实共享同一权威和事务路径。

已接受的转换契约退役了运行时 manifest/preflight 门、逐场景权威、自动硬阻塞回滚、
生产影子比较、遗留引擎和运行时引擎选择器。历史选择器配置不再是运行时输入。

### 集成点

`QueueTransitionCommand` 在每个调度器边界提交。每个边界都记录下一个边界所需的
规范事实：

| 边界 | 操作 | 必需的规范证据 |
|----------|-----------|-----------------------------|
| 消息准入（`scheduler_executor::prepare_message`） | `Claim` | 输入身份、activation 归属方、处置、权威围栏 |
| 等待恢复 | `Claim` | 精确的 wait id 和 generation、消费 activation |
| 结算（`runtime::commit_queue_settlement`） | `Settle` | 匹配的 activation、终态 Turn、WorkItem 处置 |
| 投递处置 | `Settle` | 绑定结算的 brief 或投递证据 |
| 操作者插话 | `Interject` | 运行中的 activation 和安全点身份 |
| 工作队列空闲 tick（`scheduler_continuation::emit_system_tick_from_work_queue`） | `Admit` | 可运行 WorkItem 的身份、generation 和源修订 |

语义决策平面不属于生产准入。它的生产模块和 fixture 已被移除。确定性的结构绑定和
规范协议保留了全部状态转换控制。

### 公开诊断事件流

调度器为每个经过 `append_scheduler_decision` 的决策发出一个类型化的
`SchedulerDiagnosticAuditEvent`。该事件携带：

| 字段 | 内容 |
|-------|---------|
| `decision` | `SchedulerDecisionKind` 变体 |
| `reason` | 人类可读的决策原因 |
| `boundary` | 做出决策的位置（例如 `run_loop`、`after_provider_round`） |
| `work_item_id` | 与该决策关联的可选 WorkItem |
| `message_id` | 触发该决策的可选消息 |
| `task_id` | 与该决策关联的可选任务 |
| `evidence` | 决策使用的证据字符串 |
| `scenario_class` | 可选的场景分类（例如 `operator_interjection`） |

事件通过 `RuntimeEventKind::SchedulerDiagnostic` 发出，与遗留的
`scheduler_decision` 审计事件并列。两者与调度器决策在同一事务中持久化。
类型化事件是公开的可观测面；遗留审计事件保留用于向后兼容。

### 调度建议

`SchedulingAdvisory` 是内部的、非权威的告警系统，用于发现潜在的调度器状态不一致：
空闲姿态下存在可运行工作、外部等待恢复能力薄弱、不可恢复的阻塞 WorkItem 等。
建议以 `scheduling_advisory` 审计事件追加，并与近期事件去重。

建议**不是**诊断事件流意义上的诊断。它们是供调试和运维感知使用的内部提示；
确定性的调度器投影和姿态推导仍是调度决策的唯一权威。

## 已知缺口

- `SchedulerDecisionKind` 的变体有意多于粗粒度的 RFC 姿态标签。RFC 姿态是稳定的
  轮次结束词汇；决策变体是具体的运行时动作和重复抑制结果。
- 调度器发布验收在运行时权威路径之外验证原子性、重启、有界并发负载、故障处理和
  FIFO WorkItem 投影。它既不是校准过的生产 SLO，也不能替代针对具体部署的浸泡与
  容量测试。
