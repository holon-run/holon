# Holon Agent 内存状态：步骤 3–10 总体设计与交付顺序

日期：2026-10-12；最新状态 2026-10-15。Goal active；D0–D6 已提交并审阅。D7 尚未整体交付，按用户最新指令从 D6 收敛为有实际消费者的小批次，验证和独立 review 后逐批提交，不采用 code-health-audit 流程。D8 最终 full/e2e/性能/新真实 LLM 验收及整体 draft PR 仍待完成。

代码调查基线：`codex/agent-runtime-state-rfc` / `3637f8b0ba3dd9a4c74796c50520da1b5d521b4e`。工作目录：`/Users/jolestar/opensource/worktrees/github.com/holon-run/holon/agent-runtime-state-rfc`。

本文保存在仓库外，承载总体落地设计、当前缺口、交付顺序和待确认决策。仓库内 [Agent Runtime State RFC](/Users/jolestar/opensource/worktrees/github.com/holon-run/holon/agent-runtime-state-rfc/docs/rfcs/agent-runtime-state-and-context-snapshots.md) 继续只描述方案与不变量。本文经讨论确定的新增架构合同，在对应代码实施前同步到 RFC；现状和实施记录留在本文及[实施计划](/Users/jolestar/Documents/Codex/2026-10-12/holon-agent-runtime-state-implementation-plan.zh-CN.md)。

## 1. 总体建议与已确认的四项决策

建议先确认跨域设计和验收合同，再让一个 goal 自主完成后续阶段；每阶段的具体 SQL、索引和 Rust 类型由代码审计、实验及 subagent review 收敛，不逐阶段要求用户批准普通工程选择。

最重要的顺序调整：**resident state、提交变化集和实际消费者随领域一起交付**。步骤 9 不再是第一次把领域状态装入内存的阶段。先用 Episode 验证一条完整链路，再逐域扩大；保留既有业务事务，不按阶段编号拆散其写集。

| 已确认决策 | 采用方案 | 对后续工作的影响 |
| --- | --- | --- |
| Q1：首次升级与同名身份重建如何处理旧 continuity | 首次升级显式承接现有 Agent 的所有 live work/obligations 及所需 continuity；以后同名身份重建隔离旧 continuity，旧历史仍可按已有权限显式查询。无法证明的旧半提交单独诊断，不猜测修复 | 决定 bootstrap 的 legacy 承接规则、旧 live owner 切换及身份归属测试；不能只给旧 payload 回填当前 incarnation |
| Q2：以什么规模作为这轮的硬验收负载 | 单 Agent 正确性；8 个 loaded Agent / 4 个并发请求作为正常负载，32 / 16 作为资源压力负载。使用合成历史和 scripted provider；确认新输入背压与可重试响应 | 决定基线、默认参数校准和性能通过阈值；这是建议负载，尚未证明容量 |
| Q3：升级前已接受的 pending backlog 超过新预算时如何激活 | 明确 ResourceExhausted，保留 durable 工作，调整限额后重试；暂不加入自动迁移放宽或排空模式 | 决定升级/启动兼容性；不能丢输入、偷偷 claim 冷读或无界超额加载 |
| Q4：真实 provider 成本是否属于 goal 的完成门槛 | 最新 goal 已要求真实 LLM 验收，并授权当前节点 key；工程正确性、普通零读和离线性能仍是硬门槛，真实 cache/input usage 单列受控验收，不凭少量样本宣称成本下降 | 同 route、固定脚本、隔离 Host；每组最多 12 次实际外发、输出 512 tokens/次、wire body 128 KiB/次；fixture/hash 一致不能替代真实调用 |

这四项在第 12 节展开。用户已接受 Q1–Q3 推荐，并明确要求 Q4 真实 LLM 验收；普通必要变化纳入实现与 review，不重复作为阻拦项。实施状态及证据见[goal 交付记录](/Users/jolestar/Documents/Codex/2026-10-12/holon-agent-runtime-state-goal-delivery-log.zh-CN.md)。

## 2. 目标与范围

完成后的 loaded Agent 具备一份已提交内存状态。普通 scheduler/readiness/control/wait/task/trigger 决策及 context acquisition、capture、build、budget reprojection 都读取这份状态或其一致快照。数据库负责同一事实的持久化、启动恢复、未加载查询和显式冷证据读取。

保留三个明确边界：

- 私有 candidate 在内存先计算；业务事务提交后才发布、确认成功及许可后续效果。不能让未提交状态驱动模型请求。
- provider 客户端、进程句柄、取消 token 属于 runtime resources；文件、配置和 skills 属于版本化外部观察；它们不是随 AgentState 序列化的业务历史。
- Context 是 Agent state 的投影，没有自己的写入权威；不增加第二套 context 数据库、通用图引擎或新的 scheduler 权威。

继续保持现有 prompt 选择策略、admitted input、根 operator 正文、provider protocol anchors、trust/provenance、wait generation 和执行权限。模型摘要、semantic retirement、supersedes、跨 Agent memory sharing 不进入本轮目标。内存 eviction 不删除身份或 durable history。

正常运行的“零读”指状态决策和 context 输入获取不读 DB/文件；业务提交中的 OCC/身份校验、启动加载、明确冷 recall、validated recovery 和资源刷新分别归因。用户显式要求的文件工具或网络动作保留其 I/O，不把普通 context 查询改名为工具操作来逃避验收。

## 3. 设计时基础与缺口（调查基线 3637f8b0）

本节保留总体设计时的调查，不能作为当前代码缺口清单。最新已交付边界见第 8 节及交付记录；D0–D4 已闭合的事务、恢复与 resident 消费者不重复实施。

| 当前已具备 | 后续缺口 |
| --- | --- |
| 共同 owner reservation、private candidate、known facts/publication、跨 owner 门禁与退出责任 | published observation 仍主要覆盖 AgentState/queue，尚无完整分域 resident root |
| immutable ContextSnapshot 输入与 pure build/reproject | acquisition adapter 仍读 DB/文件，且旧 snapshot token 不证明 coherent owner revision |
| 已有 queue/wait/WorkItem/task/execution 复合事务 | 提交返回物尚不包含所有受影响领域的 exact delta；普通消费者仍多处读库 |
| delivery ledger、execution command identity、task result settlement identity | 部分 witness 只能防重复，不能恢复状态推进后的原 outcome；未知提交 resolver 尚未覆盖各命令族 |
| Episode 确定性 boundary 与 typed refs | Episode/audit/builder 分开提交、同 ID 可 overwrite；详见[步骤 3 设计稿](/Users/jolestar/Documents/Codex/2026-10-12/holon-agent-runtime-state-step3-design.zh-CN.md) |
| 部分逻辑读取归因和 golden fixtures | 缺 optimized/并发性能基线、真实 SQL/file read 计数、resident/pinned 字节及真实 provider 成本 |

具体约束：`PostCommitEffects` 当前没有完整 wait/continuation/execution/settlement 和 turn/brief/tool/transcript 变化集；`persist_transcript_evidence`、`persist_tool_execution_evidence` 目前仍是 append 后 notify。若不前移这部分，后续 root 很容易只更新 queue，却遗漏同一事务改变的 wait 或证据。

Stop 也不是单一 posture 事务：当前先改变 control，再处理任务，随后独立取消 waits/timers/queue，另行撤销 triggers 和写 audit。步骤 4 必须收敛整个 Stop 的业务结果，不能只给第一笔状态更新加 receipt。

## 4. 最终内存结构与事实归属

下列是逻辑组织，不要求立即拆出全部模块或预建空框架。

```text
AgentStateStore（复用 Host 稳定 owner slot 与单写 coordinator）
  published: ArcSwap<AgentSnapshot>
    identity + existing AgentState
    execution: attempts / outcomes / admitted bindings
    inputs: pending metadata + required payloads + operator anchors
    work: WorkItems / exact waits / continuations / focus
    tasks: task facts / result settlement obligations
    scheduling: timers / wakes / external triggers
    memory: finalized Episode anchors / lookup indexes
    evidence: bounded turns / briefs / tool receipts / transcript projections
    derived indexes + coverage/version metadata
  resources: RuntimeResources
    clients / handles / cancellation / owned workers
    immutable file/config/skills/capability observations
  persistence: typed coherent load + command/commit adapters
```

已有领域记录继续拥有各自事实：ExecutionAttempt 决定 execution lane；WorkItemExecutionState 决定 eligibility；WaitCondition 决定 exact resume；queue/message 决定输入状态；Agent control 决定行政准入。索引不另存一份 runnable、focus 或等待生命周期。publication generation 是进程内 token，不新增全局业务 revision；各域既有 revision 继续负责 OCC。多个 child 的独立快照只说明各自版本，不宣称整棵 Agent tree 是同一全局时刻。

现有 AgentState 内的 working-memory/builder 字段在迁移期间保持一个权威表示；Episode domain 增加 archive records 和索引，不另建可独立修改的第二份 builder。执行 binding 与 canonical attempt 的一致性由既有协议校验，不让 legacy agent status 替代 lane authority。

每域用显式 typed maps 和共享不可变 records。candidate 共享未变域，仅复制变更记录和受影响索引。发布 root 一次替换完整兼容状态。大正文使用独立的 resident 类型或 preview/body locator，禁止用一个 `Arc<ToolExecutionRecord>` 仅为小 preview 间接保留整个巨大 stdout。

`ContextSnapshot` 只保留选中的 record/observation 引用、request binding 和 tokens。capture 结束释放 root 和 domain maps；record 不能反指 root。慢 provider 请求只能 pin 自己的准备集合，不能 pin 整代数据库。

## 5. 必须先稳定的跨域接口

### 5.1 提交变化集与原结果

引入明确的 `CommittedDomainDelta` 逻辑合同，逐域添加实际使用的字段：已提交记录、删除/availability 变化、数据库分配的 sequence，以及需要的 index/effect references。它是 canonical commit 的返回描述，不是第二套日志或通用 object registry。

```text
Applied { original_result, committed_delta, sequence_bindings }
Replayed { original_result }
Rejected / DefinitelyNotCommitted / Indeterminate
```

新命令：当前授权/身份范围 → reservation → 内存 candidate/changeset → 事务内重查 witness 与校验 → 完整写集 → 同步保存安装责任 → 原子安装/发布 → 返回结果与后续 effects。

旧命令重放：验证身份范围和原请求 → 查 receipt/witness → 返回原 outcome。不能重跑 reducer 或把历史 candidate 安装到当前 root。已执行或结果未知的非幂等外部 effect 不能因重放而再次派发；scheduler/index 通知可安全重发，未完成 durable cancellation/cleanup 义务由 reconciler 按原 run/task generation 继续，原 receipt 不表示物理效果已完成。新写入 no-op receipt 也是 durable commit，和历史 replay 分开。

同一 compound commit 无论影响几个域，都必须返回它影响的完整事实。已迁移域必须 exact install；未迁移域维持明确覆盖标记。不能先把 queue 设 Ready，再等下一次 DB 查询补 wait。跨 Agent 事务按确定顺序保留全部 owner，全部安装完成后才一起开放相关门禁。

### 5.2 Readiness 与过渡读取

区分 owner commit fence、领域覆盖、请求 dependency coverage 和资源观察 freshness。某个 Episode domain 可以用于局部投影，并不意味着整个 provider context 已 Ready。

过渡期保留显式 storage adapter，标明使用的未迁移域和原因。若组合 resident 与 durable 输入用于权威请求，必须在同一 owner reservation 下，以 coherent durable read snapshot 和冻结资源版本准备，验证与 resident root 的版本相容；做不到时只做 shadow comparison，不声称 coherent hybrid capture。持 reservation 可以做准备 I/O，不能跨 SQL 持 RuntimeAgent mutex 或短 publication gate。

目标 capture 只接受已覆盖的内存集合；缺少 mandatory source 返回 `NotReady` 或有合法 typed cause 的 `NeedsHydration`，不静默回落 storage。最终移除普通决策与 context 的 adapter capability。未加载 Agent 的 durable read 保持独立，查询不激活 runtime。

过渡与最终验收共用以下 I/O cause，所有读仍进入总成本；cause 不因缺数据而临时生成：

| Cause | 合法准备/读取边界 | 不允许扩大为 |
| --- | --- | --- |
| Bootstrap | activation/restart 时 canonical recovery 后 coherent load | 普通 turn 反复重新 bootstrap |
| Owner/WorkspaceChange | 明确切换 execution binding/workspace，准备该 scope 的 bounded closure | 同 owner 的窗口推进、relevance 变化 |
| PolicyExpansion | 显式扩大历史 policy 后重做覆盖准备 | 同 snapshot 的 budget retry/reprojection |
| ValidatedRecovery | 有 canonical interrupted/recovery binding，确需窗口外原 message/turn/predecessor | 普通 provider follow-up、工具多轮、预算重试 |
| ExplicitRecall | 经授权的显式冷 ref/history 请求 | renderer 临时找不到证据后的隐式读 |
| ResourceRefresh | 已知失效、watcher/显式刷新和首次资源观察 | 每次 dispatch 都 sync/discover/read 文件 |

事务内 OCC/身份/witness 校验是提交协议的 SQL，不是 context hydration；单独归因。普通执行必需正文在接纳时准备，普通窗口滚动由 committed evidence 增量维护，不新增 I/O cause。

### 5.3 Known/unknown 恢复

SQL 前把 typed witness descriptor 随 recovery ticket 保存：family/version、durable identity incarnation、稳定 operation scope、payload digest 和必要 invocation binding。caller 取消后 owner/Host 仍拥有恢复责任；新 owner 不借旧进程 token 获得安装权限。首次 D1 bootstrap 就复用一个 coherent durable read transaction，所有 repository loader 使用同一 connection；先完成 canonical recovery，再加载当前状态。资源 I/O 独立准备并验证 generation，不跨文件 I/O 持 SQLite writer transaction。

Known commit：保留完整 delta/root 安装事实，恢复只修内存，不重写业务 SQL。Unknown：仅由匹配该 family、ticket/group 和 digest 的 resolver 查持久化 witness；证明提交后重建当前 coherent state，再返回原结果。不能拿 latest row 当原 ACK。hydration 返回 scope/binding/version、typed availability 与字节/读数；安装复核 incarnation、依赖 revision、resource/availability generation，失配则弃置或重新规划。冷读使用 queue→目标 message、by-ID/batch-ID、目标 owner tail 和 operator anchor 查询，不先加载 ALL messages 再筛 queued，也不把全历史扫描藏进 hydration；单独测量 bootstrap/恢复峰值。witness 缺失只有在原 DB 尝试已结束、读取够新且 witness 未裁剪，并证明该 family 的所有成功分支（含 no-op）都与不可覆盖 witness 同事务时，才可判定未提交。resolver 要列明完备性、原子关联和保留证明；可选子命令、部分/可变 witness 的 absence 一律不足，继续 fenced。

不将 Episode resolver 泛化放行其他 unknown。各 family 复用现有 delivery/attempt/result identities；只有不能恢复历史结果的缺口才补 focused receipt。原 outcome、当前 root token、物理 effect 完成情况分别表达。

### 5.4 Evidence 写入和 removal

消息、turn、brief、tool receipt、transcript、Episode 在生成并完成 canonical commit 时进入 resident working set。提交适配器必须返回已分配的 message/transcript/event sequence，不能只把调用前对象放进缓存。后台排队写返回不等于提交成功；如存在此类入口，先获得提交确认再发布。

已驻留域从启用时就接上全部 writer 与 maintenance invalidation。prune/delete 返回 typed removal/availability delta，和 source/index-delete intents 同事务并由 owner 协调。D6 是补全审计，不能把 D1 已驻留数据的正确失效推迟到 D6。

## 6. Mandatory working set 与外部观察

### 6.1 准备集合与历史覆盖

准备 roots 包括：本次 admitted input 及必需正文、可信 operator anchor、execution-bound WorkItem/plan、当前 waits/pending result、选中近期 turn 的必要 evidence。validated recovery 额外包含原 message/turn/predecessor attempt，即使它们不在当前 owner 的最近窗口。

维护显式 `PreparedHistoryCoverage`：selector/policy version、execution owner、owner-tail 与 agent-tail fallback 的覆盖范围、依赖版本。当前默认 WorkItemScoped 窗口是 64、配置上限 512，基础 recent 默认 12；不能只保留 global 12 条就宣称 selector/fallback 零读。按启用策略准备可能需要的并集，普通窗口推进靠新增 commit 增量维护；owner 切换和明确政策扩大可触发受控 hydration。

依赖闭包是 typed、有限深度和有限字节的规则，不递归装入所有 lineage。不可变 record 的 ref 与 mutable current alias 分开；只有确实保存的版本才能承诺历史精确解析。optional 缺失显式 unavailable/pruned，mandatory 缺失阻止 Ready。

### 6.2 大正文、pins 与接纳

| 类别 | 规则 |
| --- | --- |
| 当前业务元数据 | loaded 时完整驻留，不按历史 LRU 丢活动 wait/task |
| 已接纳 pending input | 普通执行必需正文驻留，单独核算；接纳前按 count/bytes 背压，不能事后丢正文 |
| 普通连续性窗口 | policy 所需记录和必需正文/preview 准备齐全，随新事实滚动 |
| 大工具输出/附件 | 协议允许时保留 bounded preview 与精确 locator；所选 snapshot 使用的实际正文提前物化 |
| In-flight snapshot | pin 选中 allocation 至请求和 reprojection/continuation 结束，释放后按历史策略处理 |
| 可选冷历史 | 显式 recall 可载入，受 count/body/bytes/depth 上限约束 |

32 MiB/Agent、512 MiB/Host 仅为可回收历史的起始实验预算。当前事实、accepted payload、pins、observations、indexes、candidate 副本分别报告；共享 allocation 在 Host 物理账中去重。RSS 不等于历史预算。

超过压力阈值先回收 optional history、限制 optional hydration，再在接纳前明确拒绝或延迟新工作。已接受工作继续保留。升级启动遇到已有超限 pending 数据时不得删除或假装 Ready，建议 activation 返回明确 ResourceExhausted，保留 durable 工作，调整限额后重试；一次性迁移放宽/有界 drain 属于可选兼容方案，由 Q3 确认，不能偷偷逐条冷读或丢输入。具体限额依据 Q2 和前置测量冻结，已有 backlog 行为按 Q3。

### 6.3 外部资源

ResourceObservation 保存 source/workspace、immutable body/preview、digest、observation epoch 与 invalidation generation。已知文件写入、workspace/config 变更主动失效/更新；watcher 检测外部编辑，overflow/丢事件标脏并重同步；支持显式 refresh。

承诺 latest observed version，不承诺任意外部写入瞬间可见。mandatory resource 已知 dirty 时阻止新 capture；旧 snapshot 保持原观察。合法的已观察 Absent 与 Dirty/RefreshFailed 区分，沿用现有可选文件缺失语义，不把所有 notes/catalog 都变成启动前置。异步 refresh 安装前复核 generation/workspace/source，旧读取不能覆盖新观察。文件写入和 SQLite 不做假原子事务；plan 创建迁到 WorkItem/file workflow，render 不创建文件。

Skills/config/capability 的已有 registry/watch 机制可复用。能力撤销与 current execution fence 在 provider/effect 发送前检查，普通内容刷新不会倒改已捕获请求。动态资源基础可并行开发，但接入读路径的版本与 readiness 合同从 D0 固定。

## 7. 各领域的完整迁移边界

| 领域 | 同一 invariant 的事实与事务 | Resident 消费者 | Witness/恢复重点 |
| --- | --- | --- | --- |
| Episode | working snapshot、builder、可选 immutable Episode/source refs、epoch/count、audit/index、receipt | Episode candidate/lookup；WorkItem/wait 未迁移时保留明确 input adapter | actual dispatch turn + phase；含 no-boundary/no-op 原结果；旧半提交碰撞隔离 |
| Control | control/准入、相关 queue/wait/timer/trigger/task/execution 取消事实与义务、audit/index、原结果 | control/closure、dispatch fence、Host lifecycle 观察 | Start/Stop 稳定命令身份；旧 Stop 重放不能影响后来的 Start |
| Input/execution/evidence | message+delivery、queue claim、attempt/admitted binding、terminal/outcome、turn/brief/tool/transcript 和相关 settlement | scheduler candidate/admission、root input/anchor、history selector、provider recovery | 复用 delivery/execution witness；仅 command hash 不代表可恢复完整 outcome |
| WorkItem/focus/wait | WorkItem revision/execution generation、focus、exact wait、continuation、completion 与 queue/attempt/evidence | work queue/readiness/focus、WaitFor、context current work/plan | 保持完整原 compound transaction；prepared memory changeset 纳入原 settlement |
| Task/result | task terminal、result identity、delivery/message/queue、wait release、settlement disposition、cleanup intent/evidence | active tasks/pending results、task supervisor、context | 复用 pending→admitted→settled；结果已生成、已接纳、已结算和进程已退出分别判断 |
| Timer/trigger | fire identity/count、pending wake、wait generation、message admission/incorporation、撤销；callback accounting 保留现有分阶段合同 | timer/trigger readiness、wake/context ingress | 新 wake 的相关写集原子；已有 pending wake 时允许只推进 fire_count；Stop/replacement 竞态及 callback 分阶段恢复 |
| Maintenance | prune/removal、availability、search-index delete intent、受影响 owner | by-ref lookup、hydration、coverage/retention observations | 新 lookup 见删除；旧物化 bytes 可用但不复活 durable record |

Control 优先采用完整 canonical Stop transaction。内部命令入口一次性冻结稳定 ID，owned mutation/recovery 复用该身份；现有无客户端幂等键的 HTTP 请求继续各自代表新意图，不按 action/body/time 猜重试。第一版不增加必填 API 字段，也不宣称无键跨网络重试能返回原结果；新增可选 client command ID 的协议另行明确。它会提前使用其他域所需的最小 delta 类型和取消义务，但不要求先迁完那些域的所有功能。物理 abort/join 留在 postcommit，精确绑定旧 run/task generation；晚到的旧 Stop worker 不能取消新 Start 的工作。保持既有 public API 和 Stop/unload/identity deletion 边界；若审计证明无法保持原用户合同，再提交具体取舍讨论，不预建另一套生命周期 journal。

Timer 创建新 wake 时，Timer 推进、相关 wait trigger/message/queue/State/wake ledger 保持同事务；已有 pending wake 可合并后续 fire，合法返回 advanced=true、wake_created=false 而不新增消息。claim 验证原 wake.fire_count，不要求等于已推进的最新 Timer.fire_count。Trigger callback 的 wake 与 delivery_count 当前分两阶段，继续保留；不按 correlation_id 猜外部幂等身份，也不承诺 callback exactly-once。

Task/result 的 ensure/admit/defer/wake/owner-unavailable 等独立 writer 与 typed TaskCommand 同批迁移。现有先持久化 deferred deadline、再尝试 enqueue 的有界恢复合同可以保留；完整内存发布不要求把所有长流程硬并成一笔事务。

prepared wait/completion 的 Episode refresh 不能在 queue 已 Processed 后裸追加。D4/D5 把准备好的 memory changeset/receipt 放回原 settlement 事务；若需要后置处理，必须有 durable continuation。没有新增这一行为的路径不能标为已覆盖。

## 8. 调整后的交付顺序

| 阶段 | 对应原步骤 | 交付与退出条件 |
| --- | --- | --- |
| D0：基线与首个 resident 基础 | 前移 9 的必要部分，复用 1/2 | 补 release/并发测量；稳定 committed delta、typed witness、coverage/capture、body/pin 计量接口；只实现 AgentState/Episode 真正使用的最小 root。必须与 D1 的真实消费者一起验收，不能以空接口称完成 |
| D1：Episode 完整切片 | 3 + Episode residency | 3A 纯 planner、3B 事务/receipt、3C 恢复接入；coherent bootstrap→commit delta→resident Episode→实际投影/lookup；接通已驻留源的失效；保留未迁移 input adapter 并报告非零 I/O |
| D2：Control 与生命周期 | 4，提前取消写集所需类型 | 完整 Stop/Start 结果和取消义务、当前 dispatch fence、旧结果重放、loaded control reader；验证取消与 caller/Host 退出责任 |
| D3：输入、execution 与 evidence | 5 + message/turn/brief/tool/history 的 9 | pending payload、claim/admission/terminal 与证据产生即驻留；根输入/owner history/fallback coverage、bootstrap/recovery 和背压；相关消费者同批切换 |
| D4：WorkItem/focus/wait | 6 + 对应 9 | 完整 typed delta 和 resident 索引，切换 readiness/focus/wait/current work；适用 prepared settlement 的 memory changeset 进入原事务 |
| D5：Task/result settlement | 7 + 对应 9 | result obligation、exact waits、消息/evidence、cleanup intent 与 resident view；补齐适用 prepared memory settlement；handles 保持 transient |
| D6：Timer/trigger/maintenance | 8 + 对应 9 | 完整 fire/admission/取消协议；完成全 writer/removal 库存审计、prune/hydration/pin 竞态与 owner 失效闭环 |
| D7：全域 capture 与资源/hydration 收敛 | 9 剩余项 | 补齐资源观察与明确冷读；全部 mandatory closure 可准备；普通 loaded 决策与 capture 移除 adapter；schema/load 索引与预算调优 |
| D8：全链验收、灰度、清理 | 10 | 增长负载、故障矩阵、零读能力禁用、内存与 release 性能；按 Q4 成本验收；迁移文档和旧路径清理。达成完整停止线才结束 goal |

顺序以依赖为准。资源 observer、测量 harness 和只读比较可并行；同一业务写集/owner 安装协议串行整合。新域 reader 只有在 bootstrap、全部 writers、commit delta、maintenance 和故障恢复均覆盖后启用。D3 可先切输入/history 等已满足依赖的读者；需要尚未覆盖 wait/task 域的 scheduler/readiness 聚合继续标明 adapter，直到对应依赖闭合。不能因为一个子域已驻留就宣布整个聚合 memory-only。

每个阶段执行：局部详细设计与 inventory → 独立 subagent challenge → 实现 → 定向测试 → 非作者 review → 修复验证 → 窄 conventional commits → 更新仓库外交付记录。3B 可先提交 storage 实现，但在 resolver 未完成前不启用不完整 runtime 新路径。阶段编号不是事务边界。

2026-10-15 状态核对：D0–D6 已分别完成并提交，活动分支最后阶段提交为 D6 `d53d2707`。D7 仍在仓库外组合实现和验证，尚未交付，也未落入活动分支；D8 完整回归、性能、e2e、新真实 LLM 和整体 draft PR 均未开始。最近 exact81 的相关核心37项为34通过3失败，另一个原Result预算回归未解决。D7 交叉依赖和中间补丁展开过大，现先停止扩展新模块，修当前失败形成明确分支检查点；原完整退出条件与最终验收要求不删减。具体证据和剩余交付以[交付记录](/Users/jolestar/Documents/Codex/2026-10-12/holon-agent-runtime-state-goal-delivery-log.zh-CN.md)和[当前收敛清单](/Users/jolestar/Documents/Codex/2026-10-12/holon-agent-runtime-state-current-checkpoint.zh-CN.md)为准。

### 8.1 剩余交付的收口顺序

D5 的复杂度来自既有 Task、结果投递和物理退出分别推进。保留这些业务阶段，通过原事务回执和可恢复的后续责任衔接。可以分提交审阅，普通 reader 切换必须等待本域完整闭合。

| 顺序 | 交付内容 | 开启下一边界所需证据 |
| --- | --- | --- |
| D5-A：资格、加载与维护 | Task/Result 精确身份及版本；完整 active/unsettled 集合；有界历史；无 State、Deleting 和首次升级的既有记录维护；无效 Result→Task 关系的明确观察 | 不给 raw/foreign/pruned 记录补造权限；完整集合超预算明确失败；维护使旧视图失效，不能伪造 Ready |
| D5-B：全写入与恢复 | PreparedTask、Queue、独立 Result writers、Control、Host 启动及既有 cleanup；实际 SQL 写足迹、一次域发布、原结果和 no-op 回执 | 所有 writers/maintenance 有清单；lost ACK 返回原结果并重建当前视图；跨域事务保持原子 |
| D5-C：TaskStop 与旧数据执行衔接 | Stop intent/退出责任先持久化，再取消原代际；结果后续地址固定；child/worktree 沿既有权限与清理规则；旧 imported Turn 只作历史证据，新执行以实际 claim/run/attempt 建立 Turn | Cancel/Force 为不同新意图，重放不升级；旧 worker 不碰 successor；Task 状态、结果交付和物理退出分别验证；同一次 Begin 重试复用原 Turn/index |
| D5-D：消费者与整体验证 | context、TaskList/status/output、supervisor、scheduler reply、RunOnce 等切换；清除 Task shadow cache；与 Input/Work 同根读取 | 全 writer/reader/recovery inventory 闭合、默认栈回归、严格编译和非作者 review；局部静态通过不代表 D5 完成 |
| D6 → D7 → D8 | Timer/trigger/maintenance 全覆盖 → 资源观察与全域 pure capture、预算计量及普通 adapter 移除 → 故障/负载/e2e/真实 LLM 和整体 draft PR | 沿用第 10–11 节停止线，最终按冻结基线验证，不以局部零 SQL 或测试总数替代整体验收 |

D5-A/B/C 可在仓库外并行细化，按依赖逐片整合。TaskStop 的 focused 责任侧车属于 Task 域并共享其 revision，不增加第二套任务生命周期权威。进程句柄保留在 transient resources；Task 已 Cancelled 不能当作 OS/child 已退出或 worktree 已安全移除的证明。

### 8.2 当前需要讨论的问题

原 Q1–Q4 已确认，不重新征求批准。当前没有必须新增的产品取舍：D5 的资格类型、SQL/索引、原事务接线和有限恢复接口由实现与独立 review 收敛。

只有出现以下具体证据才再次提出讨论：既有 API/权限或保留语义必须改变；冻结负载下无法同时满足正确性与资源目标；必须操作生产 Agent/真实大库或扩大外部调用；发生无法安全兼容的用户改动。提出时给出具体失败证据、可选方案及推荐，不把尚未实现、未跑测试或普通复杂度视为阻塞。

执行状态与设计问题分开记录：此前 blocked 后已收到新的 goal 继续指令并核对 active，D5 已恢复组合验证及完整接线。活动分支仍 clean D4；D5 局部测试和阶段 review 不代表完整启用或验收。具体失败、修正及尚未验证项见交付记录。

本次补充由 Roadmap/Context 两位 subagent 只读复核，Episode 另核原交付依赖：§8.3–8.5 的 D5 完整闭合、D6–D8 顺序、计量和验收停止线保持；无新增必须由用户决策的问题。这是文档复核，不是 D5 实现验收。已有外部 patch、设计/审阅及未冻结草稿已另存[最新 D5 源码与交接快照](/Users/jolestar/Documents/Codex/2026-10-12/holon-agent-runtime-state-d5-composite-20261013-070156/README.zh-CN.md)，避免后续只依赖临时目录。

### 8.3 D5 的实际接线与提交边界

先闭合原事务的资格、写足迹、域变化集与回执，再接运行时消费者。定向 SQL 测试通过的分片可以单独审阅；启用新 reader 或 Stop 路径须满足下面的整域退出条件。

1. **写入与恢复。** PreparedTask、Queue、所有独立 Result writer、Control、启动和删除维护均使用同一 owner 协调；首次升级的 live owner/旧 Turn 衔接保持既定承接规则。没有 State、尚未加载和已经退役的身份只能进行有凭据的有限维护，不能凭原结果把 successor State 或缺失领域伪造为 Ready。
2. **TaskStop 的后续责任。** 初次事务冻结原 Task generation、Cancel/Force 意图、结果地址及完整 Work 绑定；确认提交并发布后才允许物理信号。caller 退出后由 owned worker 接管；unknown 先核原事务结果。结果投递、monitor join、child retirement 和 worktree 处置分别推进，物理退出失败不能吞掉结果责任。缺少真实退出或路径归属证明时明确保留 worktree，不以 Task Cancelled、monitor 已结束或身份退役代替证明。
3. **同根普通读者。** scheduler、context、TaskList/status/output、reply、supervisor、RunOnce 与恢复入口读取相容的 Input/Work/Task 视图；保留现有输出窗口及排序合同。具名准备仍有 I/O 时继续归因，不提前声称全链零读。
4. **去重与整体验证。** 移除 shadow Task cache 和重复恢复正文；完整 pending Stop 集合及当前代际 heads 不受历史窗口裁剪。lost ACK、原 intent 重放、后续状态推进、owner 替换、Host 关闭和原 child 的只读安装均有独立验证；最后跑默认栈回归、严格编译与非作者 review，再提交 D5。

当前新发现的 Work-owned frozen Result 字段对齐、启动夹具执行准入证明、稀疏 phase 恢复与只读 child 安装，都是上述合同下的实现修正，不构成新增产品问题。外部片的编译、静审或局部通过不能替代这些接线证据。

### 8.4 D6–D8 的细化交付顺序

| 顺序 | 主要方案 | 独立退出条件 |
| --- | --- | --- |
| D6-A：Timer/fire/wake | 迁移已有 fire、wait trigger、message/queue、State 和 wake ledger 的完整原事务；已有 pending wake 继续保留合并 fire 的合同 | 原 fire identity、wait generation、重复 callback、Stop/replacement 和 lost ACK 矩阵通过；消费者观察原子域变化 |
| D6-B：Trigger 与撤销 | 按现有 wake/delivery accounting 两阶段协议分别恢复，不扩大 exactly-once 承诺；接入当前权限及 admission fence | 所有创建/更新/撤销入口有写足迹；旧 callback 不操作 successor，既有可恢复义务不丢 |
| D6-C：Maintenance 库存收口 | 按当前 owner 协调 prune、removal、availability 和 index-delete intent；核对所有领域 writer/loader/adapter 清单 | 新 lookup 见删除；旧 snapshot 的 bytes 不复活 durable record；已驻留域的维护不能推迟到此阶段才首次接入 |
| D7-A：全域消费者与资源观察 | 消除剩余普通读取 adapter；文件/config/skills 使用带版本的显式观察，已知 dirty 阻止依赖它的新 capture | normal same-owner 窗口滚动、工具多轮和 budget retry 均无隐藏 SQL/文件读取；资源晚到/overflow/刷新失败语义通过 |
| D7-B：准备集合、pins 与预算 | 当前事实、accepted payload、所需历史和 cold recall 分开限额；补齐 D4 Work preflight/loader 临时分配观察；同一 Host allocation 去重 | mandatory closure 完整，optional 缺失明确；selected records 不持整个 root；candidate/resident/pinned/observation/index 与 RSS 分开计量 |
| D7-C：移除普通读取能力 | 对 acquisition、selection、capture、render 和 request construction 全链禁用普通 storage/file adapter；只留有真实 cause 的准备与冷读接口 | capability-deny 与真实读数交叉通过；同 owner 普通窗口变化不能改名为 Recovery 或 Bootstrap 绕过检查 |
| D8-A：正确性与故障验收 | 完整回归、跨 owner 组、cancel/lost ACK/publication 故障、重启及身份重建、权限撤销、prune/hydration 竞态 | 精确提交 SHA、全部命令族原 outcome 与当前 root 均正确；未验证项不得标通过 |
| D8-B：资源与性能验收 | 使用 D0 冻结 binary/负载和相同 release 仪器；正常 8/4、压力 32/16，增长历史及慢请求场景 | 正常 p95 不劣化超过 10%、吞吐不低于 95%；压力明确背压、accepted 工作保留、pins 可释放；bootstrap/recovery 峰值单列 |
| D8-C：e2e、真实 LLM 与整体 draft PR | 先离线全链，再按已定 Q4 同 route/隔离 Host 做真实调用，报告行为及 usage/cache；清理旧路径并汇总提交 | e2e 与真实 LLM 行为验收通过；保留实际 wire 次数/预算证据；创建整体 draft PR，状态不会自动变成 ready 或 merged |

资源观察、inventory 和测量工具可提前并行设计；新的权威读取路径依赖对应写入、维护和恢复已闭合。每个子阶段先形成具体设计与非作者 review，再编码与最小验证，只有出现新的失败证据才调整方案；不要求一次预写所有 SQL，也不把细节未定当作必须逐步请示用户的理由。

### 8.5 讨论问题的提前登记规则

当前新增待确认问题：**无**。第 1/12 节保存已确认决策，后续不再次询问相同选择。

| 若发现的具体事实 | 提交讨论时必须给出的内容 | 当前处理 |
| --- | --- | --- |
| 必须改变 API、权限、历史保留或身份删除语义 | 触发场景、兼容影响、可选方案及推荐 | 保留现有合同；普通内部接线直接修正 |
| 冻结负载下不能兼顾正确性和资源目标 | 同仪器测量、失败指标、可调参数与产品影响 | 先实测及优化，不降低验收阈值或丢 accepted 工作 |
| 必须纳入 #3503 全量回填、真实大库迁移或超出 Q4 的外部调用 | 精确目标、迁移/调用成本、恢复方案和范围增量 | 继续保持既定范围；未发生时不预建审批 |
| 用户改动与本任务发生无法安全兼容的冲突 | 精确冲突位置、可保留方案及无法兼容原因 | 不覆盖用户改动 |

## 9. 历史兼容、schema 与回退

### 9.1 身份和 legacy continuity

新的命令 key/receipt 必须含 durable identity incarnation；process owner epoch 只做执行与安装 fencing。重启查原结果不能从同名 Agent 当前 identity、turn_index 或没有生命周期字段的旧 TurnRecord 推断原 scope。

建议 Q1 的升级承接规则通过明确的 bootstrap adoption 边界记录：承接升级时所有仍 live 的 canonical WorkItem/continuation、pending inputs/results、exact waits 和 recovery obligations，以及它们的 mandatory source closure；不只承接当刻 prompt 窗口。记录允许承接的 owner/ref scope、policy/version、升级边界和来源，随后切换到另一个仍 live 的旧 owner 时，按同一 adoption scope 允许 bounded tail/closure hydration。只物化当前请求所需 working set，不同时加载所有 owner 历史；不扩大到未承接的历史身份或新身份。若使用小型 continuity manifest，它说明“本次升级明确承接”，不伪称旧 evidence 原本属于新 incarnation；不全表重写旧 payload、不把 manifest 变成另一份工作生命周期或无限 retention root。

同名身份重建后，旧历史不自动成为新身份的 mandatory continuity；已有权限允许的显式历史查询保留。来源不明的 optional 历史显式标注 legacy/unknown。旧 Episode 已写但 builder 未推进、又无 receipt 的碰撞保持 typed 恢复门禁和诊断，禁止 overwrite 或伪造成功；修复按可证明事实单独形成可 review 方案。

### 9.2 Schema 范围

本轮默认包含实现所必需的 focused receipt、identity/continuity scope、coherent load 与 outcome lookup 索引；先汇总需求，再安排 additive migrations 与必要的有限 backfill。新 scope 只能基于可证明事实或 Q1 明确承接，不按 agent_id 相同给所有历史打新身份。

[#3503](https://github.com/holon-run/holon/issues/3503) 的批量 message 反规范化仍是独立迁移议题。本轮贡献真实谓词/负载/索引需求清单，默认不顺带执行全部历史 payload 回填。若 D0 证明启动/恢复门槛依赖其 schema 调整，先提交具体批量迁移设计和成本，再决定纳入，不能隐藏为一个小 query 优化。

SQLite WAL / synchronous=NORMAL 保持既有策略；进程崩溃恢复验证不等于断电不丢已确认事务，增强这一合同需要另行讨论配置和成本。

每次 migration 给出旧数据兼容、新旧 schema、写入/升级失败恢复、大库 lock/WAL/磁盘成本。当前 RuntimeDb 已拒绝高于 binary 支持版本的 schema；读取实现回退不等于旧 binary 可原地降级。允许灰度窗口内在新 binary 切换 reader/重建视图，不承诺未经验证的旧 binary 写兼容，也不自动覆盖生产数据库。真实大库副本或线上迁移须有明确目标与授权。

Receipt 第一版按 focused family 保留原结果，不新增自动 GC；记录条数和字节增长。若要裁剪 payload，先有 retry horizon 和保留去重 fence，不能删完后把旧命令当新命令。outbox 已消费或 audit 已裁剪不影响原结果恢复。

### 9.3 灰度与回退

新旧 reader 可读取同一已提交事实做 shadow comparison，不重复消费输入、业务提交或 provider 调用。按 domain/consumer 切换，旧 reader 回退仅在尚未删除 adapter 的 cutover 灰度窗口存在；最终 process-wide 禁止普通读回退并移除该 capability。完成后恢复路径是 fence + canonical rebuild，不能继续依赖隐藏旧 reader。

发现一致性/权限问题先 fence 相关 admission/capture，核对在途 commit，再用当前 canonical persistence 重建。回退 reader 不复活旧 bypass writer。known commit 保持权威；unknown 仍先查 witness；通知失败仅修通知，资源脏只阻塞其依赖，不将恢复变成身份删除。

## 10. 验收与前置测量

最初的[步骤 0/1 验证](/Users/jolestar/Documents/Codex/2026-10-12/holon-agent-runtime-state-step01-validation/validation.zh-CN.md) 只有固定小 fixture 的 debug 数据和逻辑 operation 归因；33.206 ms capture 中位数不能充当生产性能目标，18 logical operations 不是 SQL count。随后 D0 已冻结 release/并发基线及受控真实 LLM 基线，具体原始证据见交付记录和 [D0 冻结材料](/Users/jolestar/Documents/Codex/2026-10-12/holon-agent-runtime-state-d0-frozen)。这些只是对照起点；最终实现的零读、内存、性能及真实 provider usage 尚未完成验收，不能据此宣布收益。

D0 在首次 resident 行为改造前固定：测试机/build profile、合成数据 seed、正常/压力 workload、并发、热身与样本数、计量开关，以及通过阈值。以当前 `3637f8b0ba3dd9a4c74796c50520da1b5d521b4e` 协调实现作为性能起点，原基线 `3947ddeef395d435dbcc1f55fc4522f9b2e40af2` / 步骤 0 golden 用于行为对比，两者不可混称一个版本。

建议正常负载初始阈值：capture 到 provider-request 构造 p95、commit/publication p95 相比同仪器 baseline 不劣化超过 10%，吞吐不低于 95%；统计噪声/样本不足先增加重复运行，不能改实现后倒推阈值。启动/恢复、大库迁移和压力背压另列指标，不用 warm 改善掩盖启动恶化。这些门槛已随 Q2/D0 冻结，最终新实现尚未测得通过；不得按改造后的结果倒推放宽。

| 验收维度 | 必须覆盖 | 通过证据 |
| --- | --- | --- |
| 唯一事实/写者 | loaded/unloaded、tool/API/control/recovery/Host/maintenance | 无旁路权威、无平行 runnable/focus、完整 writer inventory |
| 原子事务与结果 | 每个写点 rollback、callback retry、真实 commit 后 lost ACK、后续状态推进再重放 | 完整写集一起成功/失败；原结果可恢复，当前 root 不倒退 |
| 发布与退出 | ACK 后 install 故障、取消 caller、unknown、owner replacement、跨 owner 组 | 所有 affected gates 一致；无旧 owner 覆盖、无第二次业务补写 |
| Legacy 升级 | 当前窗口外的 pending/recovery source、多个旧 live owner 切换、重启后 adoption、同名重建 | mandatory closure 不丢；不伪造旧 incarnation；不跨承接 scope；超限按 Q3 |
| 连续性与权限 | late input、跨 owner fallback、旧 source 在窗口外、capture 后 Stop/撤权、新 budget attempt | 根输入/authority/lineage 保留；发送前重检当前执行 fence |
| 普通零读 | 新输入、工具多轮、owner-scoped/fallback、窗口滚动、长队列、预算 retry | acquisition+selection+render 全链真实 DB/file read 归因；禁用普通 read adapter 仍通过 |
| 内存 | 巨大 stdout 小 preview、慢请求、增长历史/活动事实、队列正文、并发 pins | retained/pinned/index/observation/candidate 与 RSS 分开报告；无整 root 或大 body 隐式保留 |
| Freshness/removal | 外部文件编辑/overflow、旧 refresh 晚到、prune 与 capture/hydration | dirty 可见、旧 generation 不覆盖；新 lookup 见 prune，旧 bytes 不复活 DB |
| 行为/cache | manifest、provider request shape、source refs、budget minima、真实 usage（按 Q4） | normalized 比较保留语义与 prefix 策略；真实成本结论只来自对应测量 |

SQL 计数覆盖真实 storage operation/statement 并区分读写，文件读取覆盖实际资源 reader；logical helper count 仅做补充。零读使用 capability deny 测试与归因交叉证明。bootstrap、validated recovery、显式 recall、resource refresh 和事务内校验单列原因与总成本，不能把整个 ingress 或准备阶段排除后宣布零读。

持续加入新消息与新证据，验证普通历史窗口推进不触发 hydration。验证当前策略的 WorkItemScoped 与 agent fallback 并集，不能仅跑反复使用同一 snapshot 的 fixture。每阶段运行相应定向测试，最终运行全链集成与 fault/pressure matrix、`cargo fmt --all -- --check`、`RUSTFLAGS="-D warnings" cargo check --all-targets`。记录精确 SHA、完整/分批运行和未测项目。

## 11. Goal 的执行与停止线

当前 goal 的目标是完成 D0–D8，交付包含实现、分阶段 commits、每步独立 review 修复、验证证据、RFC 方案更新和仓库外中文交接记录。沿用当前分支/worktree；最终通过 e2e 和真实 LLM 验收后，push 并创建一个整体 draft PR。当前节点 Holon API key 已获准用于隔离验收；不输出密钥。draft PR 不自动转 ready、合并或部署。

Goal 自动继续处理普通工程问题：精确类型/索引、合理拆 commit、修测试、完善 witness、解决同任务内的冲突。每阶段先细化完整写集与 reader 清单，再实施；subagent 的建议由主 agent 核验，不把“review 无发现”当运行验证。

需要再次讨论的边界：改变产品/权限/保留语义，无法在已定负载合同内满足正确性与资源目标，必须纳入未授权的大库/外部操作，或发现无法安全兼容的用户改动。普通代码协调和临时 fixture 不重复询问。

完成必须同时满足：所有目标域和 loaded 运行消费者覆盖；正常 warm 决策与 context 零读；原命令结果可恢复；内存计量与负载门槛通过；旧读写路径清理；按最新 goal 完成真实 LLM 验收并报告实际 usage/cache；没有测得成本下降不能声称下降。不能因阶段耗时、局部测试通过或 token 预算将 goal 标完成。

每阶段把 SHA、writer/reader 覆盖、schema、测试结果、剩余 adapter、已知风险和下一动作写回仓库外工作记录。需要跨时间等待外部事项时进入持久跟踪；当前 goal 已启动，阶段证据持续写入仓库外交付记录。

## 12. 已确认取舍及具体建议

**Q1：legacy 承接与新身份隔离。** 推荐首次升级承接所有仍 live 的 work/pending/wait/continuation/recovery obligations 及 mandatory closure，并留下明确承接 scope；保留当前策略窗口，后续旧 live owner 切换按同一 scope 有界准备；同名身份重建从新 continuity 开始。显式历史查询按原权限保留，旧歧义不猜测修复。替代选择是严格只载入有生命周期证明的记录（升级可能减少历史），或允许同名重建续接旧历史（需要显式继承授权协议）。

**Q2：代表性规模与性能门槛。** 建议正常测试为 8 个 loaded Agent、4 个 scripted 并发请求，每 Agent 100 条 pending 输入、平均正文 16 KiB、10 万条合成历史记录；另测 10 MiB 工具输出仅保留小 preview。压力测试扩到 32 / 16 并逐步增长输入，检验 backpressure、pins 释放与恢复；不要求在资源不足时仍无条件接纳。采用第 10 节初始相对阈值，D0 测量前冻结。新输入在超限前明确背压并给 caller 可重试响应；既有超限 backlog 的 activation 行为独立由 Q3 确认。该规模已确认；只有实测发现目标冲突时，才提出带证据的调整方案。

**Q3：既有超限 backlog 的激活。** 推荐 activation 返回明确 ResourceExhausted，保留全部 durable 输入与身份，诊断所需/可用预算，允许调整限额后重试；不加入自动超额加载。替代方案是一次性有界迁移预算/排空模式，必须独立保证峰值、完整正文和退出条件，不能用普通 claim 逐条冷读替代。这个选择决定升级后某些 Agent 能否立即激活，与 Q2 的测试规模是不同问题。

**Q4：真实 provider 成本验收。** 工程验收与真实成本结论分开，不承诺成本下降；真实 LLM 行为验收是最新 goal 的完成门槛，用户已授权复用当前节点 Holon API key。基线/新实现使用同 route、固定脚本和隔离 Host；初始每组最多 12 次实际 wire sends、单次输出最多 512 tokens、最终 wire body 最多 128 KiB，禁用或计入所有 retries/fallback。输入 token 是估计/响应后统计，字节上限不冒充精确计费上限；实际 usage/cache 单列。生产 Agent 不承载实验。未执行真实调用时，文档只能写“request/cache 策略回归通过，实际 usage/成本未测”，不能据此结束 goal。

以上没有要求重新批准已定的单写者、commit-before-publication、latest-observed freshness、保留 accepted input 或身份删除权限边界。#3503 默认仅纳入需求汇总；整体方案接受后，具体方案落到 RFC，现状与实施阶段继续放仓库外。

## 13. 本次核对的材料

- 当前架构：[Agent Runtime State RFC](/Users/jolestar/opensource/worktrees/github.com/holon-run/holon/agent-runtime-state-rfc/docs/rfcs/agent-runtime-state-and-context-snapshots.md)、[Commit Contract](/Users/jolestar/opensource/worktrees/github.com/holon-run/holon/agent-runtime-state-rfc/docs/rfcs/runtime-transition-commit-contract.md)、[Unified Execution Protocol](/Users/jolestar/opensource/worktrees/github.com/holon-run/holon/agent-runtime-state-rfc/docs/rfcs/scheduler-work-item-unified-execution-protocol.md)、[Host Read/Admission](/Users/jolestar/opensource/worktrees/github.com/holon-run/holon/agent-runtime-state-rfc/docs/rfcs/runtime-host-activation-admission-and-read-projections.md)。
- 当前源代码：[ContextSnapshot/adapters](/Users/jolestar/opensource/worktrees/github.com/holon-run/holon/agent-runtime-state-rfc/src/context/snapshot.rs:104)、[scheduler projection](/Users/jolestar/opensource/worktrees/github.com/holon-run/holon/agent-runtime-state-rfc/src/runtime/scheduler/projection.rs:93)、[commit result](/Users/jolestar/opensource/worktrees/github.com/holon-run/holon/agent-runtime-state-rfc/src/runtime_db/transitions.rs:124)、[owner commit/publication](/Users/jolestar/opensource/worktrees/github.com/holon-run/holon/agent-runtime-state-rfc/src/runtime/state_mutation.rs:150)、[Stop](/Users/jolestar/opensource/worktrees/github.com/holon-run/holon/agent-runtime-state-rfc/src/runtime/lifecycle.rs:333)、[evidence producers](/Users/jolestar/opensource/worktrees/github.com/holon-run/holon/agent-runtime-state-rfc/src/runtime.rs:6038)、[schema version fence](/Users/jolestar/opensource/worktrees/github.com/holon-run/holon/agent-runtime-state-rfc/src/runtime_db/mod.rs:1353)。
- 本轮只读刷新 GitHub：[#3478](https://github.com/holon-run/holon/issues/3478) / [#3503](https://github.com/holon-run/holon/issues/3503) 为 open；[#3467](https://github.com/holon-run/holon/issues/3467) / [#3225](https://github.com/holon-run/holon/issues/3225) 已 closed，保留为 recovery operator root 和 cache 成本回归案例。没有新增 issue 评论或外部写入。

2026-10-13 再次只读核对 #3478/#3503，仍为 open，更新时间分别为 2026-10-10 / 2026-10-12，未发现改变本方案边界的新内容。#3478 的模型语义操作和跨 Agent memory 仍是额外需求；完成本轮状态改造不能自动把原 issue 全部验收项标完成。

本轮由三位 subagent 分别审阅 resident/读取边界、领域事务/恢复及交付依赖。建议修订后均复核无剩余实质阻塞；这是静态设计 review，尚非实现验证。调查、原审阅与处理结果保存在[总体设计审阅记录](/Users/jolestar/Documents/Codex/2026-10-12/holon-agent-runtime-state-overall-design-review/review-resolution.zh-CN.md)。
