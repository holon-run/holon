# Agent loop 与输入处理的简化方案

2026-11-20 更新（初版 2026-11-14）· 讨论稿，非规范性 · 代码基线 `origin/main` `ba79e53a5`（初版基线 `90811078`）

这篇是供讨论的方案，不是已接受的设计。它回答一个问题：怎样让 agent loop 以及各类输入源（以 TaskResult 为首个切片）的分类、调度与恢复逻辑更容易推理，从结构上减少 #4074、#3620 及跨 turn 丢失唤醒这类缺陷。

#4095 和 #4101 已经修掉了 #4074 的直接缺陷：`Defer` 改为窄写，无法推进的结果不再每 30 秒写库，两条没有出口的状态有了结局；#4134 解决了跨时钟复用；#4230 废弃了 `NeedsRepair`；#4228 和 #4247 进一步把 Task 重准备迁出共享 writer 并按实际依赖跳过普通 live 输入的 Task 准备。这篇讨论的是这些修补背后的整体结构，目标是让 **整个 agent loop 的输入处理** 在设计上保持单一权威、封闭触发条件与零空闲成本。

方案的核心是三件事：

1. **所有待处理项的“下一步”由统一的纯函数决定。** 无论是队列消息、未结算 TaskResult、到期 Wait、挂起的 ProviderRecovery 还是可运行的 WorkItem，都先由纯函数分类为 `Wake`、`Attach`、`Reduce`、`Hold(until)`、`Settle` 或 `Park`，每个状态都有明确出口。
2. **等待由事件驱动，消除多套独立的扫描与合成消息乒乓。** 触发条件是一个封闭集合；loop 不再由多个扫描阶段合成 `SystemTick` / `InternalFollowup` 写入队列再由下一阶段读回，也不再分散维护五套睡眠期限。
3. **loop 只做“事件 → 决定 → 应用”，常驻视图与请求形状和时间无关。** 空闲的 agent 不读库也不写库。

这不是新的调度器，也不是推倒重来。[Scheduler–WorkItem Unified Execution Protocol](../rfcs/scheduler-work-item-unified-execution-protocol.md) 已经为执行准入定下了“每个问题只有一个权威”的原则；配套草案 [Agent-Owned Execution and WorkItem Attribution](../rfcs/agent-owned-execution-and-work-item-attribution.md) 将 **WorkItem 的工作调度状态机** 与 **Turn / Task / TaskResult / Wait 的所有权** 解耦。这篇 notes 说明如何按同一原则收口 TaskResult 账本、其他输入源以及 agent loop 本身。

## 暴露出的结构问题

证据来自 #4074 的复核评论、#4094，以及对当前主循环（`ba79e53a5`）的逐源梳理。

### 1. 主循环是“四阶段 + 合成队列消息乒乓 + 五路睡眠期限”

当前主循环（[runtime.rs](../../src/runtime.rs#L7002-L7154)）每一轮依次执行四个阶段：

1. **TaskResult 恢复扫描**（[emit_due_task_result_recovery](../../src/runtime/task_result_recovery.rs#L738)）：扫描到期的结算账本，逐条判断资格后合成 `InternalFollowup`（`subsystem = "task_result_recovery"`）写入队列，然后 `continue`。
2. **Agent Wait 重检**（[emit_due_agent_wait_recheck](../../src/runtime/scheduler_continuation.rs#L556)）：扫描带 `recheck_at` 的 `WaitCondition`，合成 `SystemTick`（`subsystem = "wait_condition_recheck"`）写入队列，然后 `continue`。
3. **调度轮询**（[poll_with_waiting_replies](../../src/runtime/scheduler_executor.rs#L975-L1055)）：读取队列，在内存里跳过 `provider_parked` 与 `WaitingReplyQueue`，把选中的消息经 [canonical_activation_candidate](../../src/runtime/scheduler.rs#L760) 和 [resolve_canonical_activation_scenario](../../src/runtime/scheduler.rs#L961) 映射到 11 种候选和多种激活场景，再执行准入、降级（`ReduceOnly`）、挂起或丢弃。
4. **空闲兜底与休眠**（[runtime.rs](../../src/runtime.rs#L7062-L7153)）：当队列返回 `Idle` 时，再调用 [maybe_emit_pending_system_tick_with_replies](../../src/runtime/scheduler_continuation.rs#L147) 检查可运行的 WorkItem、`work_item_recheck` 与 `pending_wake_hint`，合成 `SystemTick`（`subsystem = "work_queue" | "work_item_recheck"`）写回队列；若无事可做，再汇总**五个独立的期限源**取最小值休眠：
   - `next_provider_queue_recheck_at`（[queue_parking.rs](../../src/runtime_db/provider_admission/queue_parking.rs#L459)）
   - `next_work_recheck_at`（[work_reads.rs](../../src/runtime/work_reads.rs#L263)）
   - `next_agent_wait_recheck_at`（[work_reads.rs](../../src/runtime/work_reads.rs#L279)）
   - `next_queued_available_retry_at`（[scheduler_continuation.rs](../../src/runtime/scheduler_continuation.rs#L531)）
   - `next_task_result_probe`（[task_result_recovery.rs](../../src/runtime/task_result_recovery.rs#L92)）

这形成了统一的模式性复杂度：**调度器内部为了给自己发信号，先合成假消息写进 SQLite 队列，再在下一轮从队列里读出来反序列化 JSON 重新分类**；同时每类暂时不可投递的输入都各自长出了一套退避状态（`task_result_probes`、`provider_queue_parking`、`WaitingReplyQueue`、`work_recheck_at`、`wait.recheck_at`、`queued_available_retry`）。

### 2. 结算账本成了第二个调度器（最先暴露的切片）

统一执行协议规定，结算账本“而不是第二个执行或唤醒权威”来记录待处理、已准入和已结算的责任。

实现里，`emit_due_task_result_recovery` 自己做了一遍调度：事故时每 30 秒为每条无法推进的结果写一条持久化的 `Defer`，线上 94 条结果的重检需求约是 writer 全部容量的 4 倍。#4101 之后改为内存退避并用提交 effects 提前唤醒（[wake_task_result_probes](../../src/runtime/task_result_recovery.rs#L110)），写入问题缓解了，但扫描仍然是独立阶段，一条等待中的结果同时由持久化期限、内存探测记录和已发布元数据三处描述。

### 3. 同一个问题有很多处在回答，且受限于 Owner 耦合

“这条输入现在能不能进模型、投给谁”分散在约 12 个函数与约 30 个 `canonical_*` 拒绝原因里。这些局部规则各自正确，但组合起来存在死路：

- **被丢弃的回复**（#4074）：调度认领因 WorkItem 不可运行而丢弃回复，结算义务保留并只允许精确 waiter 认领，扫描却认为“原始队列项仍拥有首次投递权”，线上积压 73 条。
- **`invalid_or_stale`**（#4074）：义务分类返回该值后扫描直接延期，走不到后面的结算分支。
- **跨 turn 替换 wait 丢失 TaskResult 唤醒**：Turn 1 `WaitFor(task1)` → Turn 2 被另一事件唤醒并 `WaitFor(task2)`（取消了 `wait1`）→ `task1` 结束。由于按 owner 门控，会话/生命周期 owner 会因 `anchor_turn_live == false` 将 `task1` 结算为 `invalid_or_stale` 静默丢弃，WorkItem owner 则因 `Waiting(wait2)` 无限期卡住 `task1`。

配套草案 [agent-owned-execution-and-work-item-attribution.md](../rfcs/agent-owned-execution-and-work-item-attribution.md) 从根源上把 **WorkItem 调度状态机**（管工作续接）与 **Turn / Task / TaskResult / Wait 所有权**（归 Agent）拆开，直接消除了约一半的跨 owner 判定分支。

### 4. 常驻快照按请求形状发布、重准备落入写事务

每个 agent 的 Task 域只有一份已发布的 coverage，包含 `required_keys`、terminal 窗口、查询选择和 `recovery_at`。#4134 修复了仅因 `recovery_at` 推进导致的每次重建，#4228 将重准备迁出共享 writer，#4247 按实际依赖跳过了普通 live 输入的 Task 准备；但 coverage 中仍然保留请求形状字段和时间字段，且 Work、Input、Scheduling 域是否也有同类形状耦合尚待统一收口。

### 5. 规模

`src/runtime/` 约 15.6 万行，`src/runtime_db/` 约 18.4 万行（不含测试），调度执行器 7,400 行，`host.rs` 3.3 万行，运行库有 165 张表。每多一处独立的合成消息阶段和退避镜像，就多一组需要对齐的校验与测试夹具。

## 输入源清单与统一分类

下表基于 `ba79e53a5` 代码核对，覆盖主循环处理的全部 12 种 `MessageKind`、11 种 `CanonicalActivationCandidate` 与启动恢复路径：

| 类别 | 具体来源 | 现状入口与路径 | 现状独立阶段 / 退避状态 | 目标统一分类（`ItemStep`） |
| --- | --- | --- | --- | --- |
| **Operator 输入** | `OperatorPrompt`（含 `Priority::Interject`） | 入队；空闲时经 `ExplicitlyBoundOperatorInput` / `ExactWaitResume` / `LifecycleExternalNudge`（[scheduler.rs](../../src/runtime/scheduler.rs#L900-L919)）；运行中 `Interject` 在安全点附着当前 turn（[turn/execution.rs](../../src/runtime/turn/execution.rs#L1079)） | 无独立扫描；受 `provider_queue_parking` 影响 | 空闲：`Wake`（最高优先级）；运行中且为 `Interject`：`Attach`；通道占用且非 `Interject`：`Hold(AttemptSettled)` |
| **外部带内容输入** | `ChannelEvent`、`WebhookEvent`、`CallbackEvent` | 入队；有权威 wait 关联走 `ExactWaitResume`，否则走 `LifecycleExternalNudge`（[scheduler.rs](../../src/runtime/scheduler.rs#L921-L937)） | 无独立扫描 | 匹配 wait 或带内容可准入：`Wake`；无权限/过期关联：`Reduce` / `Settle` |
| **定时器** | `TimerTick` | 触发入队，走 `ExactWaitResume`（匹配活跃 wait）或 `LifecycleExternalNudge`（[scheduler.rs](../../src/runtime/scheduler.rs#L939-L947)） | 启动时 `recover_active_timers`（[runtime.rs](../../src/runtime.rs#L8437)） | 到期触发：`Wake`；已取消/过期：`Reduce` |
| **Wait 重检** | `WaitCondition.recheck_at` | `emit_due_agent_wait_recheck` 合成 `SystemTick(wait_condition_recheck)` 入队（[scheduler_continuation.rs](../../src/runtime/scheduler_continuation.rs#L556-L635)） | 独立阶段 2 + `next_agent_wait_recheck_at` | **不再合成消息**：到期直接作为 `Wake` 候选；未到期 `Hold(Deadline(recheck_at))` |
| **TaskResult 与子代理回复** | `TaskResult`、`is_agent_message` 回复 | 入队 + 持久化结算账本；认领走 `ExactTaskRejoin` / `UnboundTaskResultWaitOrReduce` / `ExactWaitResume`（[scheduler.rs](../../src/runtime/scheduler.rs#L861-L898)），部分回退 `ReduceOnly` 交由恢复扫描再合成 `InternalFollowup(task_result_recovery)` | 独立阶段 1 + `task_result_probes` + `WaitingReplyQueue`（[scheduler_executor.rs](../../src/runtime/scheduler_executor.rs#L328)） | **首个切片**：按新 RFC 归属 Agent，正常/失败/外部取消为 `Wake`（可搭车），模型自身 `StopTask` 为 `Attach`；不再合成恢复唤醒消息 |
| **WorkItem 工作续接与阻塞重检** | `Runnable` WorkItem、blocked recheck、`pending_wake_hint` | 队列空闲时由 `maybe_emit_pending_system_tick_with_replies` 合成 `SystemTick(work_queue \| work_item_recheck)` 入队（[scheduler_continuation.rs](../../src/runtime/scheduler_continuation.rs#L147-L260)），再映射 `WorkItemAutonomousContinuation` / `BlockedRecheck`（[scheduler.rs](../../src/runtime/scheduler.rs#L765-L839)） | 独立阶段 4 + `next_work_recheck_at` + `next_queued_available_retry_at` | **不再合成消息**：`decide` 在无更高优先级事件时直接生成 Work-driven `Wake` 候选；重检/退避归入 `Hold(Deadline)` |
| **Provider 恢复与队列挂起** | `InternalFollowup`（含 provider recovery provenance）、被 park 的消息 | `ProviderRecovery` / `UnboundProviderRecovery`（[scheduler.rs](../../src/runtime/scheduler.rs#L840-L850)）；可用性拒绝写入 `provider_queue_parking` | `provider_queue_parking` 表 + `provider_parked` 集合 + `next_provider_queue_recheck_at` | 到期且预算内：`Wake`；退避期内：`Hold(Deadline(park_until))`；epoch 耗尽：`Park`（已由 #4210 收口） |
| **其他内部跟进** | 普通 `InternalFollowup` | `InternalFollowup` / `LifecycleExternalNudge`（[scheduler.rs](../../src/runtime/scheduler.rs#L851-L860)） | 无独立扫描 | 合法绑定：`Wake`；过期/无目标：`Reduce` / `Settle` |
| **纯归约 / 非模型信号** | `TaskStatus`、`Control`、`BriefAck`、`BriefResult` | 入队后在 [message_dispatch.rs](../../src/runtime/message_dispatch.rs#L447-L452) 固定走 `reducer_only_dispatch = true`（[L509-L540](../../src/runtime/message_dispatch.rs#L509-L540)），永不进模型 | 无 | 一律 `Reduce`（有界批处理，不进模型、不抢占模型准入逻辑） |
| **启动恢复** | 姿态、控制面义务、监督子任务、定时器、保留认领 | 启动一次性执行 `reconcile_posture_observation`、`reconcile_loaded_control_obligations`、`recover_supervised_child_tasks`、`recover_active_timers`、`recover_scheduler_bootstrap_claims`（[runtime.rs](../../src/runtime.rs#L8373-L8448)） | 仅启动一次 | 保持为启动期一次性重建，不进入常驻 loop |

各输入源之间真正需要保留的业务差异只有四个维度：**信任与来源**（`origin` / `trust` / `priority`）、**是否可进模型**（`Wake` / `Attach` vs `Reduce`）、**是否精确关联某个 wait**、以及**义务载体**（队列项、结算账本或 WorkItem 状态）。这四个维度在统一分类函数里全部作为显式输入，而不再通过“不同的扫描循环 + 合成消息”来表达。

## 已有的方向

方案建立在以下结论之上：

| 来源 | 结论 | 这里怎么用 |
| --- | --- | --- |
| [统一执行协议](../rfcs/scheduler-work-item-unified-execution-protocol.md) | 每个问题一个权威；候选是临时的纯值；准入和结算各是一个事务 | 准入路径不变，所有模型执行仍只经过 `AdmitExecution` |
| [统一执行协议](../rfcs/scheduler-work-item-unified-execution-protocol.md) | 新增一个准入权威必须在同一变更里替换掉一个旧的 | 不新增持久化的候选、预留或镜像表；逐步删除合成消息与重复扫描 |
| [Agent-Owned Execution and WorkItem Attribution](../rfcs/agent-owned-execution-and-work-item-attribution.md)（draft） | `WorkItemExecutionState` 只管工作驱动调度；Turn / Task / TaskResult / Wait 归属 Agent | 消除 TaskResult 按 WorkItem / `anchor_turn_live` 拦截的门控，支持跨 wait 非消耗性唤醒 |
| [恢复预算](../rfcs/task-result-recovery-budget.md)，#4101 修订 | 无法推进的义务在内存里退避；相关的已提交变化可提前唤醒 | 推广为所有输入源共用的封闭 `Trigger` 集合 |
| [结果内容与呈现](../rfcs/task-result-content-and-presentation.md)，#4101 修订 | owner 暂时等待时回复留在原队列项上；历史被丢弃回复停止自动重检，仍可由精确 waiter 认领 | 纳入 `ItemStep` 判定表 |

常驻状态层的四条前提不变：

- 读不取写所有权，直接读已发布的根。
- 读准备不进入写事务（#4228 已推进 Task 域）。
- 类型化的并发冲突是可重试的，不是致命错误。
- 已加载 agent 的当前状态在启动时完整加载，之后只由已提交的增量维护（要求 coverage 去形状化）。

## 目标与非目标

目标：

- **统一分类、完整出口**：任一待处理输入（队列消息、TaskResult、Wait 重检、Provider 恢复、WorkItem 续接）在任一时刻都有且只有一个确定的 `ItemStep`，不存在无出口状态。
- **消除内部消息乒乓**：loop 不再靠“扫描阶段合成 `SystemTick`/`InternalFollowup` 写入队列 → 轮询阶段读回”来驱动调度。
- **无进展则无写入，空闲零成本**：没有新事件、没有到期期限的 agent，loop 不读库、不写库；睡眠期限由统一的 `Hold` 索引给出。
- **形状无关**：常驻视图不记录调用方的请求形状，也不记录时间。
- **交互优先**：operator 的写不排在后台恢复写后面。

非目标：

- 不引入多通道并发执行，不替换 provider turn loop。
- 不改变信任、来源、权限的判定，不放松精确 wait 的安全规则。
- 不改变“先持久化再投递给模型”和提交后才发布的顺序。
- 不新增持久化的生命周期镜像。
- 不在第一步一次性改造所有输入源：按切片推进，先落地 TaskResult，再依次收口 Wait 重检、Provider 挂起和 WorkItem 续接。

## 方案

### 1. 统一的输入分类函数与首个切片（TaskResult）

对 agent loop 看到的每一类待处理项，引入统一的纯分类结果：

```rust
enum ItemStep {
    /// 可作为独立来源启动一次 attempt（也可在同轮被捎带）。
    Wake { via: AdmissionSource },
    /// 仅附着到正在运行或即将启动的 attempt（如运行中 Interject、模型自身 StopTask 的结果），不单独唤醒空闲/睡眠 agent。
    Attach { mode: AttachMode },
    /// 纯确定性状态归约（如 TaskStatus / Control / BriefAck / 过期关联清理），不进模型。
    Reduce { action: ReduceAction },
    /// 现在不能推进。`until` 列出能改变这个结论的事件，不能为空。
    Hold { reason: HoldReason, until: Vec<Trigger> },
    /// 不会再投递，用已有的处置值结算。
    Settle { disposition: Disposition },
    /// 运行时无法自行推进（如预算/epoch 耗尽），停止自动处理并让 operator 可见。
    Park { diagnostic: Diagnostic },
}
```

约束：

- **穷举**：输入是有限的枚举组合，函数体是对它们的穷举匹配，新增一个状态而不处理会编译失败。
- **`Hold` 必须可解除**：`until` 里的每个触发条件来自同一个封闭枚举 `Trigger`，并且有测试证明每一种都能由某个已有的状态转换产生。
- **不写库**：函数只返回结论，由调用方用已有的转换去应用；所有模型准入仍只走 `AdmitExecution`。
- **时间是参数**：`now` 由调用方传入，不存进任何快照。

#### 首个切片：TaskResult 的 `next_step` 判定表

结合 [agent-owned-execution-and-work-item-attribution.md](../rfcs/agent-owned-execution-and-work-item-attribution.md) 已确认的设计决策，TaskResult 归属 Agent（`work_item_id` 仅作归属标签，不再按 WorkItem 状态或 `anchor_turn_live` 拦截结果），判定表收敛为 10 行：

```rust
fn next_step(result: &ResultFacts, agent: &AgentFacts, now: Time) -> ItemStep
```

| 结果与 Agent 的状态 | 结论（`ItemStep`） | 说明 |
| --- | --- | --- |
| 结果依赖关系或持久化 Task 关联无效 | `Settle(InvalidOrStale)` | 与现状一致 |
| 义务无效或过期，证据不足 | `Park` | 不再自动重试 |
| 回复已被别处消费 | `Settle(ReplyConsumedElsewhere)` | 与现状一致 |
| Agent 控制姿态为 `Stopped` 或 `Paused` | `Hold(until: [AgentControlChanged])` | 不靠 WorkItem 状态隐含覆盖 |
| Agent 存在打开的 `ExecutionAttempt`（通道占用） | `Hold(until: [AttemptSettled])` | 通道空闲后重新判定 |
| 回复队列项仍在排队等待首次投递 | `Hold(until: [ReplyQueueEntryTerminal])` | 保留原消息首次投递权 |
| 回复队列项已丢弃，且无精确 waiter | `Hold(until: [ExactWaiterRegistered, OperatorRearm])` | 停止自动重检，等待显式接管 |
| 处于恢复退避期内 | `Hold(until: [Deadline(backoff_until)])` | 唯一保留的 TaskResult 期限类等待 |
| 恢复预算用尽 | `Park` | 发布诊断并暂停自动处理 |
| 模型在后续 turn 自身 `StopTask` 取消，且无匹配 active wait | `Attach(PiggybackOnNextTurn)` | 只搭车下一轮 turn，不单独唤醒 |
| 其余情况（无论 WorkItem 处于 `Runnable` / `Waiting` / `Paused` / `Completed`） | `Wake(RuntimeEventReentry)` | 可直接唤醒并进入 LLM，不消耗无关 `Active` wait；每轮最多捎带 8 条 |

原来因 WorkItem / Turn owner 耦合存在的 `owner_closed`、`owner_missing`、`owner_not_runnable`、`owner_has_unresolved_wait`、`claimed_by_other_waiter`（同 Agent 跨 WorkItem）、`anchor_turn_live` 终结收敛以及“已有恢复唤醒在排队”分支全部移除。

### 2. 事件驱动的等待（全 loop 共用的封闭触发集合）

所有输入源的 `Hold` 共享同一个封闭的 `Trigger` 枚举：

| 触发条件（`Trigger`） | 由什么产生 | 覆盖的输入源 |
| --- | --- | --- |
| `AttemptSettled` | 终态结算（通道空闲） | 队列消息、TaskResult、WorkItem 续接 |
| `AgentControlChanged` | 控制命令（start / stop / pause / resume） | 全部输入源 |
| `WaitChanged` | `WaitFor` 注册、触发、解决或取消 | Wait 重检、ExactWaitResume、被丢弃回复 |
| `WorkItemStateChanged` | 结算、WorkItem 工具、控制命令 | WorkItem 续接、blocked recheck |
| `QueueEntryTerminal` | 认领、丢弃、隔离 | 回复消息、队头阻塞项 |
| `Deadline(at)` | 统一时钟（仅限显式期限/退避：timer、`wait.recheck_at`、provider parking、TaskResult 退避、blocked recheck） | 替代原来的 5 套独立期限查询 |
| `OperatorRearm` | 控制命令 | `Park` 或长期 `Hold` 项 |

原则：

- **触发条件由 `Hold` 声明，不靠唤醒逻辑猜原因字符串。**
- **非期限类的等待没有定时器。** 睡眠只需要查统一内存索引里最早的 `Deadline(at)`，不再在 `RunLoopPoll::Idle` 里分别调 5 个 `next_*_recheck_at`。
- **内存索引由启动时只读跑一遍分类重建**，并保留低频只读核对（如每 10 分钟）作为漏事件的兜底与缺陷探针。
- **长期等待靠年龄发现**：超过阈值（如 24 小时）仅发诊断，不按年龄改状态；只有触发条件已不可能发生时才 `Settle` 或 `Park`。

### 3. loop 变成“事件 → 决定 → 应用”

把现在的四个阶段和五路期限合并为一次循环体：

```text
loop {
    等待：提交通知（带本次变化的 Trigger 集合）| 最早的 Hold::Deadline | 关闭
    view   = 已发布的根                           // 不读库
    action = decide(view, changed_triggers, now)  // 纯函数：分类所有受影响项并选择下一步
    apply(action)                                 // 单次窄命令：Reduce / Settle / AdmitExecution / Sleep
}
```

`decide` 的选择顺序（单次决策，不写中间队列消息）：

1. **有界 `Reduce` / `Settle`**：先处理非模型归约项（`TaskStatus`、`Control`、`BriefAck`、`BriefResult`、过期清理）和终态结算，不启动模型。
2. **Event-Driven `Wake`**（按 trust / priority / exact-wait 排序）：Operator 输入 > 已触发的 `WaitCondition` > 可唤醒的 `TaskResult` / `ProviderRecovery` / 外部带内容事件。命中后在同一 `AdmitExecution` 事务里捎带最多 8 条 `Wake` / `Attach` 状态的 `TaskResult`。
3. **Work-Driven `Wake`**：无事件驱动来源且通道空闲时，从 `WorkItemExecutionState::Runnable` 中按焦点规则选出续接候选（同样捎带最多 8 条可投递 `TaskResult`），不再先合成 `SystemTick(work_queue)` 入队。
4. **`Sleep(earliest_deadline)`**：没有可执行动作时，取所有 `Hold(Deadline)` 的最早时间进入休眠；没有期限则纯等通知，零读库、零写库。

### 4. 常驻视图去形状化（先在 Task 域验证，再推广）

在“完整加载加增量维护”的前提上加三条：

- **coverage 只包含活动集合**：活动 Task、未结算结果及其关联 Task，由每次写入产生的增量维护（#4095 完成 `Defer`，#4228 完成重准备迁出 writer 与增量收尾）。
- **历史按需读取**：terminal 窗口、查询选择、按 key 查找通过有界只读查询获取，不属于 coverage，也不参与相等比较。
- **coverage 不含形状和时间**：去掉 `required_keys`、terminal 窗口大小、查询选择和 `recovery_at`，时间始终作为参数 `now` 传入。

验证通过后，再核查 Work、Input、Scheduling 域是否保留了同类形状字段，按相同方式收口，保证 loop 的 `decide(view, ...)` 读已发布根时零重建。

### 5. 写入车道

- writer 队列分交互和后台两级，交互写优先。
- 后台结算可以合并：同一个 agent 的多条 `Settle` 在一个事务里提交。
- 纯读移出 writer（#4087 已在推进）。

### 6. 需要修订的契约

| 文档 | 要补的内容 |
| --- | --- |
| [Agent-Owned Execution and WorkItem Attribution](../rfcs/agent-owned-execution-and-work-item-attribution.md) | 确立 WorkItem 调度状态机与 Turn / Task / TaskResult / Wait 所有权解耦的模型，收录 10 行 TaskResult `next_step` 判定表 |
| [统一执行协议](../rfcs/scheduler-work-item-unified-execution-protocol.md) | 同步 `WorkItemExecutionState`（移除 `InFlight`）与统一的输入分类 / 事件驱动 loop 契约 |
| [恢复预算](../rfcs/task-result-recovery-budget.md) | “Scanner convergence”一节改为事件驱动的重新决定；非 admission 的等待必须有触发条件或上界 |
| [agent-runtime-state-and-context-snapshots](../rfcs/agent-runtime-state-and-context-snapshots.md) | coverage 不含请求形状和时间 |

## 迁移步骤

每一步可以独立合并和回滚，前一步不依赖后一步；先用 TaskResult 跑通模式，再把其余输入源并入同一个 `decide`。

| 步骤 | 内容 | 状态 | 验收 |
| --- | --- | --- | --- |
| 0 | 计数与特征基线：Task 准备按调用方和未命中原因归因；判定表特征测试 | #4101、#4134 已完成归因计数；跨 turn 替换 wait 与新判定表特征测试随步骤 3 落地 | 线上能回答“哪个调用方、为什么重建” |
| 1 | 修复用条件：带恢复来源的调度读在事实未变时复用已发布快照 | #4134 已完成 | 状态不变时连续两次空闲调度读，第二次 Task 域加载为零 |
| 2 | 无法推进的结果不写库，两条死路有出口 | #4101 已完成（#4230 另已废弃 `NeedsRepair`） | 60 条不可投递的结果，writer 每分钟持锁低于 1 秒 |
| 3 | **切片 1（TaskResult + RFC Phase 1）**：解耦 TaskResult 的 WorkItem / `anchor_turn_live` 门控，引入 `ItemStep`、`Trigger` 与 10 行 `next_step`（先影子对比再切换），加低频只读核对并去掉非期限定时器 | 未做 | `WaitFor(task1)` → `WaitFor(task2)` → `task1` 终态可唤醒并进入 LLM，同时保留 `wait2`；空闲 agent 在 10 分钟内零读写 |
| 4 | **切片 1 收尾**：TaskResult 其余通道迁移到 `next_step`，删除重复判定函数和 `task_result_recovery` 合成唤醒消息 | 未做 | TaskResult 判定逻辑只剩一处；特征测试全部通过 |
| 5 | **常驻视图去形状化**：Task 域 coverage 去掉形状字段与 `recovery_at`，完善增量维护（并核查 Work / Input / Scheduling 域） | #4095 完成 `Defer`，#4228 迁出重准备，#4247 按依赖跳过普通 live Task 准备；完全去形状化待收尾 | 写入不触发全量重建；增量结果与全量重算逐字段相等 |
| 6 | **切片 2（其余输入源并入 loop）**：把 `emit_due_agent_wait_recheck`、`maybe_emit_pending_system_tick_with_replies`、`provider_queue_parking`、`WaitingReplyQueue` 依次并入统一 `decide` 与单一 `Hold::Deadline` 索引，消除内部 `SystemTick` 合成消息；writer 分交互/后台两级 | 未做 | 主循环收敛为单阶段；五路期限归一；饱和负载下 `POST /prompt` 的 writer 排队 p95 低于 100 毫秒 |

注：步骤 6 中的 **writer 分级** 与步骤 3–5 正交，如果线上指标显示 operator 写仍在排队，可以提前单独交付；步骤 3 之前先配合 #4222 收敛测试夹具初始化成本，以降低多 PR 迭代验证开销。

## 风险

- **漏事件导致输入卡住。** 事件驱动把正确性押在“每个 `Trigger` 都有人发出”上。缓解是启动时重算、低频只读核对，以及“每种触发都能被产生”的测试。
- **增量维护的正确性。** 用 #4095 已有的方法验证：增量安装后的常驻域与全量重算逐字段相等。可在调试构建里保留影子重算。
- **消除合成 `SystemTick` 时影响依赖审计/队列观测的测试。** 现有不少测试（如 `scheduler_continuation.rs` 中直接调用 `maybe_emit_pending_system_tick` 的用例）依赖合成消息的存在；步骤 6 应先保留审计事件语义、逐类切换候选生成，再删旧合成路径。
- **与进行中的工作冲突。** #4074 / #4189 / #4222 的后续切片会碰同一批文件，需按小 PR 顺序推进，每个 PR 删除被替代的旧判定，避免长期停在半迁移状态。

## 需要讨论的问题（已确认）

2026-11-18 更新：结合 `WaitFor(task1)` → `WaitFor(task2)` → `task1` 终态唤醒场景，已形成草案 [Agent-Owned Execution and WorkItem Attribution](../rfcs/agent-owned-execution-and-work-item-attribution.md) 并确认全部核心决策：

| 问题 | 结论 |
| --- | --- |
| 1 投递与唤醒规则 | `TaskResult` 归属 Agent（`work_item_id` 仅作归属标签）。除模型自身 `StopTask` 产生的 `Cancelled` 仅搭车（`Attach`）外，其余终态结果均自带唤醒权（`Wake`），可在不消耗其他 `Active` wait 的前提下唤醒 Agent；任何 Turn 启动时最多捎带 8 条可投递结果；即便 WorkItem 已 `Completed`，结果仍带标签投递 |
| 2 长期等待 | 年龄只发诊断，不按年龄改变状态；触发条件不可能发生时由分类函数直接 `Settle` 或 `Park` |
| 3 terminal 窗口 | 倾向移出 coverage，在步骤 5 前把 `current()` 窗口改为 0 用测试确认真实依赖方 |
| 4 资源记账 | 不移除，限定在加载边界；分类函数只读已发布的根，不触发准备 |
| 5 步骤 1 | 已由 #4134 完成 |
| 6 契约位置 | 架构与所有权边界入 [agent-owned-execution-and-work-item-attribution.md](../rfcs/agent-owned-execution-and-work-item-attribution.md)，并同步修订统一执行协议与恢复预算 RFC |
| 7 `WorkItem.blocked_by` 与 `WaitFor` 替换 | `blocked_by` 仅为自然语言说明，不作调度门控；`WaitFor` 默认替换同作用域旧 wait，靠 TaskResult 的 `Wake` 保证此前任务终态仍能唤醒 Agent |

## 局限

- 主循环四阶段、五路期限与 12 种 `MessageKind` / 11 种 `CanonicalActivationCandidate` 的路径已在 `ba79e53a5` 代码中核对；但步骤 6 对 `Work`、`Input`、`Scheduling` 三个常驻域的 coverage 去形状化细节尚未逐文件盘点。
- “约 12 处判定函数”“约 30 个拒绝原因”与规模数字沿用初版基线 `464c5e498` 梳理结果，未在 `ba79e53a5` 重算。
- 事故数据取自 `7164c2f08` 和 `464c5e498`；#4101、#4134、#4228、#4247 合并后的线上综合指标尚未重新采集。
