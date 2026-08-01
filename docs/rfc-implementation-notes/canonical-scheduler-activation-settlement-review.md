# Canonical Scheduler Activation/Settlement 实现审计

本文审计 canonical scheduler 中 activation、settlement 及其相邻控制状态的
当前实现。它记录的是实现层面的维护性判断，不是新的调度契约，也不替代
[Runtime Scheduler Contract](../rfcs/runtime-scheduler-contract.md)、
[Agent Activation, Settlement, and Dispatch](../rfcs/agent-activation-settlement-and-dispatch.md)
或 [Scheduler Cutover Simplification](../rfcs/scheduler-cutover-simplification.md)。

审计基线：`a0a4d8c8`，2026-08-01。

## 结论摘要

Canonical scheduler 不是一套应当整体废弃的方案。以下基础能力仍然具有明确的
长期价值：

- 纯函数 reducer 和显式状态转换；
- typed command、typed conflict 和 fail-closed 检查；
- SQLite 事务内的原子提交、幂等命令结果和 append-only audit；
- activation generation、dispatch revision、wait generation 等 fencing；
- origin、trust、priority、binding 和 provenance 的显式保存；
- 可重放、可诊断、跨重启恢复的协议事实。

不值得按当前形态长期维护的，是 activation/settlement 同时作为“执行尝试证据”和
“独立调度控制面”的职责组合。当前模型让 activation、slot、dispatch、focus、
work demand、wait、settlement 和 missing settlement 共同决定一次执行能否开始或
结束，而 Queue、Run、Turn、WorkItem 和 runtime wait 又保存着同一生命周期的业务
事实。这种重复使一个业务转换必须跨多个聚合保持一致，并将本可恢复的状态偏差
升级为 agent 无法运行、消息停留在队首或持续重启。

因此，本审计建议保留 canonical protocol 的基础设施和不可变证据能力，但收窄
activation/settlement 的长期职责：activation 表达一次已准入的 execution attempt，
settlement 表达该 attempt 的终态 outcome；运行资格、等待所有权和下一步 runnable
状态应由一套唯一的生命周期事实决定，而不应再由 activation/settlement 与另一套
WorkItem/queue/wait 状态共同裁决。

这不是立即删除 activation/settlement 的决定，也不是另起一套 scheduler 的提案。
在新的简化设计被 RFC 明确之前，现有 canonical reducer 仍然是生产契约，修复必须
继续遵守它的事务、幂等、fencing 和 provenance 边界。

## 当前实现形态

### 一个 snapshot 承载多组相互约束的事实

`src/domain/scheduler_protocol.rs` 中的 `Snapshot` 同时保存：

- `slot`：当前 running activation；
- `dispatch` 和 `dispatch_revision`：agent lane 是否开放或被某个 wait 预留；
- `focus`、`work`：聚焦 WorkItem 及其 runnable/waiting/terminal demand；
- `waits`：wait identity、generation、owner、trigger 和 consuming activation；
- `activations`、`activation_admissions`、`admitted_generations`、
  `activation_inputs`：执行准入及其 fence、输入和 provenance；
- `settlements`、`missing_settlements`：执行终态及恢复要求；
- `continuation_admissions`：完成后向调用方转移 generation 的记录。

这些字段不是相互独立的审计索引。`assert_invariants()` 需要交叉验证 owner、slot、
dispatch、work、wait、activation、settlement、continuation 和 recovery 之间的大量
组合关系。当前该函数约 850 行，本身已经说明协议正确性依赖全局状态组合，而不是
少数可以局部验证的状态转换。

### Planner 和 settlement builder 仍需读取协议外业务事实

`SchedulerProjection` 不只读取 canonical work/wait 状态，还组合 agent status、
queue length、active run、Task、WorkItem read model、waiting intent、timer、turn terminal
和 runtime error 等事实。

`SchedulerDecisionExecutor::canonical_activation_plan()` 在生成 protocol command 前，
仍需从 WorkItem storage、work queue projection、resolved wait condition、task binding
以及 message continuation 中判断 activation owner 和 admission shape。

反方向上，`canonical_queue_settlement_commands_from_facts()` 又从 queue entry、message、
terminal turn、WorkItem、wait condition、brief 和 agent focus 推导 settlement；无法唯一
推导时写入 `MissingSettlementRecord`。这表明 settlement 并不是执行结束时天然产生的
唯一事实，而是事后从其他生命周期事实重建出来的协议事实。

### 实现规模不是问题本身，但反映了状态面的宽度

以下六个调度相关核心模块当前合计 15,421 行，不含主要集成测试和 fixture：

| 模块 | 行数 |
| --- | ---: |
| `src/domain/scheduler_protocol.rs` | 5,249 |
| `src/runtime/scheduler.rs` | 1,957 |
| `src/runtime/scheduler_executor.rs` | 1,657 |
| `src/runtime/scheduler_acceptance.rs` | 2,511 |
| `src/runtime_db/transitions/scheduler_protocol_repository.rs` | 2,621 |
| `src/runtime/waiting.rs` | 1,426 |

最初引入确定性 Scheduler / WorkItem 协议的提交 `7202af23` 修改了 52 个文件，新增
24,755 行。行数不能单独证明过度设计，但结合随后集中出现的状态交接、恢复和重启
问题，可以确认维护成本主要来自跨层一致性，而不是单个 reducer 分支写得不够严谨。

## 为什么当前 activation/settlement 聚合不适合作为长期控制面

### 1. 同一个执行生命周期存在多套真相

一次消息执行至少会经过 Queue claim、Agent run、Turn、WorkItem/Wait、Activation 和
Settlement。每一层单独看都有合理用途，但当前它们都参与控制判断：

- Queue 判断消息是否仍可 claim、是否 processed 或 interrupted；
- Run/Turn 提供实际执行与 terminal evidence；
- WorkItem/Wait 表达业务工作是否 runnable、waiting 或 completed；
- Activation slot 和 dispatch 再次表达 agent 是否运行及 lane 是否保留；
- Settlement 再次表达工作继续、等待、完成和 dispatch disposition。

当不同层对同一转换的提交时机不同，系统必须依赖 adoption、recovery 和 invariant
把它们重新拼合。问题并不是“事实很多”，而是没有一套事实能单独回答“agent 现在
为什么可以或不可以执行下一条消息”。

### 2. 一个业务转换被拆成前置条件互相冲突的命令

[#2476](https://github.com/holon-run/holon/issues/2476) 是最直接的例子。一次
`LifecycleExternalNudge` 在已有 lifecycle wait 上运行，turn 内创建并等待 WorkItem：

1. `settle_lifecycle()` 对 `WorkContinues/Open` 的 lifecycle nudge 有意保留原 lifecycle
   dispatch reservation；
2. 随后的 `adopt_activation_work_state()` 要求 dispatch 为 `Open`，或者已经是目标
   WorkItem 自己的 wait；
3. 两条命令放在同一事务中仍会因为
   `activation_work_state_dispatch_not_open` 回滚；
4. 重启后旧 reservation 继续存在，新的 WorkItem activation 又会被
   `agent_lane_reserved` 拒绝。

这里缺失的不是另一个 guard，而是一个明确的 lifecycle-wait → WorkItem-wait 原子
业务转换。只要 settlement 和 adoption 分别维护各自完整的控制语义，就会持续产生
这种“每条命令局部正确，组合后不可达”的状态。

### 3. `AgentLifecycle` owner 扩大了 activation 的职责边界

Activation 最初很适合表达某个 WorkItem generation 的一次执行尝试。但 operator
input、external nudge、无 WorkItem 的 command task 等输入也需要运行，于是协议增加
`SchedulerOwner::AgentLifecycle`。从此 activation 既可能属于 WorkItem，也可能属于
agent lifecycle。

这解决了“所有模型调用都必须有 canonical activation”的完整性问题，却引入了新的
所有权交接：一次 lifecycle activation 可以在 turn 内创建、聚焦或等待 WorkItem，
随后必须把 dispatch、wait 和 focus 从 lifecycle owner 原子迁移到 WorkItem owner。
该交接不是边缘情况，而是长期运行 agent 中 operator nudge、任务创建和等待的常规
路径。

如果一个抽象需要表达所有执行入口，但实际工作所有权会在一次执行内部改变，那么
它更适合作为 attempt envelope，而不是持续拥有后续 runnable/waiting 状态的聚合根。

### 4. Missing settlement recovery 形成了循环依赖

Settlement 的设计目标之一是让 activation 终态显式、可恢复。但当前 settlement 经常
需要从 queue、turn terminal、WorkItem scheduling state、active wait、brief 和 focus
反推。证据不唯一时，系统先记录 missing settlement，再由 recovery 读取同一批外部
事实生成修复命令。

这形成了循环：其他生命周期事实用于推导 settlement；settlement 和 dispatch 又用于
决定其他生命周期事实能否继续执行。`SettlementMissing` 因而不只是审计上的“不完整
记录”，而会参与 lane reservation、recovery admission 和下一次 activation 的合法性。

长期更稳健的边界应是：primary lifecycle transition 成功提交后，同一事务写入 attempt
outcome；即使 outcome 索引缺失，也能从 primary facts 确定 agent 是否 runnable，缺失
记录只触发诊断或幂等补写，不应成为调度停机条件。

### 5. 状态空间超过了局部推理和穷举测试的能力

当前正确性同时取决于：

- 两类 owner；
- slot idle/running；
- dispatch open/awaiting 及 revision；
- WorkItem runnable/waiting/terminal、metadata revision 和 scheduling generation；
- wait active/triggered/consumed/resolved；
- activation running/settled/settlement-missing/recovery；
- 多种 cause、binding、continuation 和 restart checkpoint。

Reducer 测试对单条命令和许多已知冲突覆盖较好，但真实事故发生在跨所有权、跨事务、
跨 runtime restart 的组合路径。#2476 已明确指出，已有 adoption 测试从
`dispatch=Open` 开始，没有覆盖真实的 lifecycle reservation handoff。

继续为每个组合补 guard 和 fixture 可以提高局部覆盖，却不能改变组合数量持续增长的
事实。长期降低缺陷率需要减少 authoritative state dimensions，而不只是扩充
`assert_invariants()`。

### 6. Canonical planner 仍依赖另一层 operational projection

Canonical admission 不是只基于 protocol snapshot 做决定。它还需要读取 message、Task、
WorkItem、wait condition、agent state 和 queue projection，settlement builder 也需要
读取这些事实。这意味着 canonical protocol 当前更像覆盖在已有 runtime lifecycle
之上的控制层，而不是唯一状态机。

这不等同于仍在运行两套 scheduler；accepted cutover RFC 已经要求一个进程只选择一个
调度引擎。问题在于：即使只有 canonical engine，engine 内仍存在 protocol state 与
operational state 的双向同步。只删除 rollout/shadow 控制面并不会自动消除这层重复。

### 7. Protocol conflict 会演化为 agent 不可用

Typed conflict 和 fail-closed 是值得保留的安全能力。但当前许多 conflict 位于普通运行
必经路径，且失败后的 containment 不足：

- `canonical queue settlement has no matching activation` 曾导致 bounded restart loop；
- unbound task rejoin 曾让 queue head 无法通过 admission；
- wait ambiguity 曾阻塞 restarted message；
- stale focus adoption 曾让 agent 启动失败并持续重启；
- lifecycle/WorkItem handoff 冲突会留下持久 lane reservation。

Fail-closed 应阻止错误所有权或重复执行，但不应默认把局部状态偏差提升为整个 agent
运行循环的 fatal error。一个长期运行的 headless runtime 需要明确区分：拒绝当前命令、
保留或重排消息、隔离冲突 WorkItem、进入可诊断 hold，以及真正停止 agent。

### 8. 当前产品尚未兑现部分资源调度复杂度的收益

`WorkDemand` 已包含 capabilities、locks、locality 和 cost class，协议也预留了复杂的
owner、continuation、preemption 和 recovery 语义。但当前主要运行形态仍是一台 daemon
内每 agent 单 lane 的串行模型执行。

为未来分布式资源调度保留扩展点是合理的；让这些扩展点现在就进入 admission、snapshot
和 invariant 的核心交叉约束，则提前支付了长期维护成本。按照项目“small、explicit、
easy to reason about”的约束，应先让单 lane queue/wake/sleep/task lifecycle 清晰稳定，
再由真实需求扩展资源匹配。

## 事故与修复证据

下表不是说所有问题都由 activation/settlement 单独造成，而是展示重复生命周期事实和
跨层交接如何反复成为故障放大器。

| Issue | 状态 | 暴露的边界问题 |
| --- | --- | --- |
| [#2394](https://github.com/holon-run/holon/issues/2394) | Closed | Queue settlement 找不到 matching activation，恢复路径变成 hard error。 |
| [#2407](https://github.com/holon-run/holon/issues/2407) | Closed | Task rejoin 无条件要求 WorkItem binding，无法表达合法的 lifecycle-owned task。 |
| [#2415](https://github.com/holon-run/holon/issues/2415) | Closed | Restarted message 被 wait ambiguity 检查挡在队首。 |
| [#2443](https://github.com/holon-run/holon/issues/2443) | Closed | 需要引入 missing-settlement recovery，把外部 terminal facts 重建为 canonical settlement。 |
| [#2445](https://github.com/holon-run/holon/issues/2445) | Closed | Cutover/adoption 时 WorkItem、wait、task rejoin facts 不完整，产生 hot retry。 |
| [#2460](https://github.com/holon-run/holon/issues/2460) | Closed | Canonical focus 与 operational focus 分叉，adoption conflict 导致重启循环。 |
| [#2475](https://github.com/holon-run/holon/issues/2475) | Open | Read API 隐式创建 runtime，产生无 activation owner 的 orphan dequeued claim；这是 ingress/lifecycle 边界问题，也暴露 recovery 对 matching activation 的依赖。 |
| [#2476](https://github.com/holon-run/holon/issues/2476) | Open | Lifecycle wait 到 WorkItem wait 的两段命令前置条件冲突，持久 reservation 阻断后续 activation。 |

这些 issue 的共同模式是：系统能识别不一致并 fail closed，但无法总是在不停止 agent 的
前提下把状态收敛到一个明确、可继续的结果。

## 应保留的 canonical 能力

后续简化不应退回隐式、不可审计的调度逻辑。至少应保留以下边界：

1. **确定性 transition**：同一状态和命令得到同一结果，不在 reducer 内读取时间、存储
   或 provider。
2. **事务原子性**：queue claim、primary lifecycle transition、attempt evidence 和
   outbox/audit 在一个 SQLite transaction 中提交，或全部回滚。
3. **稳定幂等 identity**：重试、重启和 recovery 使用稳定 command/attempt identity，
   同 identity 同 payload 返回既有结果，不同 payload 产生 typed conflict。
4. **Generation fencing**：旧 task result、旧 wait trigger 和旧 activation 不得修改新的
   lifecycle generation。
5. **Owner 和 provenance**：WorkItem、agent lifecycle、operator、external channel 和
   internal follow-up 不得丢失来源、trust 或 binding。
6. **不可变执行证据**：activation admission 和 terminal outcome 作为审计记录保留，
   可用于 replay、诊断和计费，但不与 primary lifecycle state 争夺控制权。
7. **失败隔离**：ambiguous evidence 仍应 fail closed，隔离到具体 message、attempt 或
   WorkItem，并提供稳定诊断和显式恢复入口。

## 建议的职责收缩方向

本节描述目标边界，不决定具体 schema 或迁移步骤。任何实现前仍需更新 RFC。

### 1. 确立唯一的运行资格事实

选择一套最小生命周期状态，单独回答：

- 当前是否有 execution attempt 正在运行；
- 哪个 WorkItem 或 lifecycle input 拥有它；
- agent 为什么在等待，以及什么事件可以唤醒；
- 当前是否有 runnable work；
- terminal transition 是否已经原子提交。

Queue/WorkItem/Wait/Turn 与 activation slot/dispatch/work demand 之间不能继续双向互为
authoritative。其他表可以作为索引、审计或 rebuildable projection，但不能再次阻止
primary state 已经允许的执行。

### 2. Activation 收缩为 execution-attempt envelope

Activation 应至少保存 attempt id、owner、input、origin/trust/priority、binding、
admitted generation、started-at 和 idempotency fence。它可以证明“哪次输入以什么权限
启动了哪次执行”，但不应长期复制 WorkItem runnable/waiting 状态或单独持有后继工作。

`AgentLifecycle` 可以继续作为 attempt owner，前提是 turn 内创建 WorkItem 后不需要在
两个长期聚合之间迁移同一个 dispatch reservation。WorkItem 的后续等待应由 primary
WorkItem/wait lifecycle 原子接管。

### 3. Settlement 收缩为 terminal attempt outcome

Settlement 应记录 completed、waiting、yielded、interrupted 或 failed 等终态以及必要的
brief、turn、wait、task 和 next-work 引用。它应与 primary terminal transition 同事务
生成，而不是在 queue processed 之后依赖多处投影反推。

Missing settlement 可以保留为完整性诊断：它触发幂等补写、告警或 repair report，
但只要 primary lifecycle facts 足够明确，就不应保留 lane 或阻止下一次 admission。

### 4. 将 handoff 表达为一个业务转换

在职责收缩完成前，类似 lifecycle wait → WorkItem wait 的交接也必须是一个 reducer
command 和一个 transaction：同时验证 source owner/generation、消费旧 wait、安装目标
WorkItem/wait、更新 focus/runnable state、释放或替换 dispatch reservation，并写入
settlement evidence。不得通过放宽“任意 reserved lane 可被 adoption 覆盖”来绕过
owner 检查。

### 5. 把 conflict containment 纳入 scheduler contract

每类冲突必须有明确的运行结果，而不只是 error code：

- stale/duplicate：幂等返回或丢弃旧 trigger；
- target temporarily unavailable：保留 queued，并设置有界 recheck；
- owner/binding conflict：隔离具体 message 或 WorkItem，保持 agent 可处理无关输入；
- ambiguous recovery evidence：进入 recovery hold，暴露诊断，不执行 provider/tool；
- invariant corruption：停止该 partition，其他 agent 和只读 API 保持可用。

Runtime loop 不应把普通 protocol rejection 无差别转成 agent restart。

## 已采用的近期收缩

在不更换 scheduler、不修改持久化 schema 的前提下，近期实现先收缩最容易反复出错的
lane 边界：

- settlement 中的 dispatch disposition 保留为 attempt 结束时的不可变证据，不再通过
  扫描历史 settlement 推导当前 lane；
- 当前 lane 只校验实时 dispatch、当前 wait generation、WorkDemand 和 activation slot；
- legacy adoption、activation adoption、WorkItem settlement 和 lifecycle settlement 通过
  同一个内部 lane transition 同步 resolve/rearm wait、WorkDemand 与 dispatch revision；
- legacy compatibility adoption 按 WorkItem 隔离，单个 stale/rejected candidate 留在
  canonical partition 之外并产生诊断，不再阻止 agent 处理已经合法的 canonical work；
- canonical prestate 损坏、存储错误和 missing-settlement recovery 仍然 fail closed。

这是面向稳定性的有界修正，不代表 activation/settlement 的长期职责收缩已经完成。后续
是否继续简化，应以生产 trace replay、restart drill 和 invariant 规模是否明显下降为依据。

## 验证和测试缺口

本次复核的 focused tests 结果为：

```text
cargo test --all-targets scheduler -- --nocapture
  100 passed

cargo test --test scheduler_workitem_mvp --test scheduler_lifecycle_owner
  41 passed
```

这些测试证明已有 reducer、repository 和 runtime scheduler 行为具备较多回归覆盖，但
不能证明组合状态已经充分验证。本次没有把完整 `cargo test --all-targets` 记为通过；
此前运行进入与 scheduler 无关的慢速/远端测试后被中止。

后续测试应优先覆盖状态序列，而不是继续只增加单命令 happy path：

1. **模型化状态机测试**：生成 command sequence，每一步验证 invariant，并检查任何
   rejection 都不修改 snapshot。
2. **Crash-point 测试**：在 claim、admission、turn start、wait registration、terminal
   transition、settlement/outbox commit 前后分别终止，重启后验证 exactly-once ownership。
3. **所有权交接矩阵**：lifecycle→WorkItem、WorkItem→continuation caller、task rejoin、
   operator interjection、wait resume，覆盖 dispatch open/awaiting 和 active/triggered/
   consumed wait。
4. **乱序与重复输入**：旧 generation task result、重复 timer、重复 callback、重复 queue
   delivery 和同 id 不同 payload。
5. **持久化属性测试**：reducer snapshot、normalized SQLite rows、reload snapshot 三者
   等价；事务失败不得留下部分 slot/dispatch/wait/settlement。
6. **故障容错测试**：protocol conflict 不得形成无界 queue-head retry、无界 agent restart
   或永久 dequeued claim。
7. **长期 soak/stress**：多个 agent 并发、SQLite busy、进程 SIGKILL、连续 wait/wake、
   compaction 和 restart；检查无 orphan claim、无 reserved lane 泄漏和无重复 brief/tool。
8. **生产 trace replay**：把 #2394、#2407、#2415、#2460、#2475、#2476 的最小事件序列
   固化为 deterministic fixtures，确保修复后状态可收敛且 agent 继续运行。

验收不应只看“所有测试通过”，还应持续检查以下安全属性：

- 每个 agent 最多一个 live execution attempt；
- 每个 dequeued claim 必须对应 live attempt，或能在重启时证明为 orphan 并恢复；
- 每个 active wait 只有一个 owner 和 generation；
- terminal WorkItem 不保留 focus、dispatch reservation 或 runnable demand；
- settlement/recovery 缺失不会让已经明确的 primary lifecycle state 失去可运行性；
- 任意重试和重启不重复 provider call、tool execution、delivery 或 brief。

## 后续决策门槛

在决定是渐进收缩现有聚合，还是重塑 canonical scheduler 内部状态之前，需要新的 RFC
明确回答以下问题：

1. 哪一组表/事件是 queue、running、waiting、runnable 和 terminal 的唯一权威事实？
2. Activation/settlement 是控制状态、审计记录，还是二者中有严格边界的一部分？
3. 哪些 conflict 只隔离当前 work，哪些才允许停止 agent partition？
4. 现有数据库如何升级或 rebuild，是否需要兼容历史 activation/settlement replay？
5. 单 lane runtime 当前真正需要哪些资源调度字段，哪些推迟到有实际需求时再引入？

在这些问题决策完成前，不应再增加新的 owner 类型、adoption 分支、recovery authority
或平行 lifecycle cache。短期 bug 修复应优先形成一个原子业务转换，并补充跨重启的
端到端回归测试。
