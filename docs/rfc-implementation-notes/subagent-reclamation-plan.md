# Subagent 清理与运行体回收：调研及实施计划

状态：实施与验证中；本文件是可随实现更新的计划，不是运行时契约。

关联 RFC：[`rfc-subagent-residency-and-reclamation`](../rfcs/subagent-residency-and-reclamation.md)。
关联需求：[#3430](https://github.com/holon-run/holon/issues/3430)。

调研日期：2026-10-09。代码基线：
[`93fa91260fbf`](https://github.com/holon-run/holon/commit/93fa91260fbfd1dce484b4e585c2014f360b9c0f)。
第二节的文件和符号是改造前快照；最新实施状态见第七节。调研结论来自源码检查，
没有执行现网清理，也没有通过压力测试确认内存、进程或磁盘泄漏。

## 一、实施结论

保留现有创建和复用方式。parent 判断 child 不再需要后直接请求删除；
后台发现可能被遗忘的清理责任，给 parent 合并提醒；runtime 校验并执行。
另外增加安全的闲置运行体退出，身份和上下文仍可复用。

首期复用现有 daemon 删除协调器承载轻量扫描、提醒调度和回收任务管理，
复用已有删除 job 执行器完成实际删除。暂不新增独立的监控 agent、
常驻后台服务、通用 maintenance 框架或 parent 复核协议。

## 二、当前实现调研

| 位置及符号 | 已具备的能力 | 本次需要补齐或验证 |
| --- | --- | --- |
| [身份与监督契约](../rfcs/agent-identity-relations-and-message-delivery.md)：`AgentSupervision` | parent 拥有 child 生命周期责任；停止或保留不会关闭监督关系 | 将已有责任接到直接、受限的 parent 删除入口 |
| [工具注册](../../src/tool/names.rs)、[工具实现](../../src/tool/tools/mod.rs) | 有创建、调用和查询 agent 的内建工具 | 未找到直接的 `DeleteAgent` 内建工具；这不等于 operator 没有删除能力 |
| [host](../../src/host.rs)：`begin_public_agent_deletion` | operator 控制面校验目标，创建删除 job，卸载 runtime，唤醒协调器 | 不能直接当作 parent 授权接口；需要专门的监督关系和工作保护准入 |
| [删除仓库](../../src/runtime_db/repositories.rs)：`AgentDeletionAdmission` | 按版本准入，以及一次性终态 child 的专门准入 | 增加 parent 请求类别；不可伪装为 operator 或终态一次性任务 |
| [任务执行](../../src/runtime/tasks.rs)、[任务归约](../../src/runtime/task_state_reducer.rs) | `ChildAgentTask` 默认终态删除；`ActorInvocation` 默认保留；有结果与删除联合提交路径 | 保留现有语义，核对 parent 直接删除后的结果和产物可访问性 |
| [删除协调器](../../src/deletion.rs)：`run_daemon_deletion_coordinator` | 单个 daemon executor；事件唤醒、启动恢复、30 秒兜底、持久重试；历史残留扫描每批 16 个 | 复用服务，但新增公平调度，不能把长任务直接塞进扫描分支 |
| 同文件：`drain_due_deletions` | 每批有数量限制，满批后让出执行权并继续排空 | `yield_now` 不会让外层循环执行提醒扫描；需改为每轮有限批次或受管执行 future |
| [host](../../src/host.rs)：`AgentEntry`、`HostRuntimeRegistry`、`activate_agent` | 已有 runtime generation、加载注册和恢复管理 | 增加 idle retirement 的准入与旧 handle 隔离；不再建一份 registry |
| 同文件：`unload_runtime` | 从 registry 移除，abort 并 await 主 task | 不能据此断言所有工具、进程和克隆 handle 已释放；不可直接用作安全休眠 |
| [生命周期](../../src/runtime/lifecycle.rs)：`request_service_shutdown` | 通过 scheduler 执行服务关闭 | 原因是 daemon shutdown，可能中止执行；增加明确的 idle exit 语义 |
| [DB retention](../../src/runtime_db/retention.rs)、host retention loop | 按策略清理审计、transcript、tool execution；WAL checkpoint 独立运行 | 继续专管数据库维护，不承担 child 生命周期或模型提醒 |

已有 `Fence / Quiesce / Ingress / Scheduler / Workspace / Index / Home / Finalize`
删除链和历史残留扫描。缺口主要在 parent 请求准入、清理责任提醒、安全闲置退出。
现有 Quiesce 可以终止任务，Home 可以移除目录，因此必须先做好 parent cleanup 的
保护检查，不能靠破坏性阶段把“有工作”的对象变成“可清理”。

调研还核对了三项外部依据：

- [Codex 源码快照](https://github.com/openai/codex/blob/50d77959bf/codex-rs/core/src/agent/control/legacy.rs)：
  live shutdown 包含持久化、等待退出和移除驻留；关闭持久 spawn edge 是另一项操作。
- [Claude Code 官方文档](https://code.claude.com/docs/en/sub-agents#resume-subagents)：
  已完成的 subagent 可继续使用；运行结束与上下文保留是不同问题。
- [Cloudflare 官方文档](https://developers.cloudflare.com/agents/runtime/execution/durable-execution/)：
  执行取消与 settled rows 清理分开；协议终态本身不证明执行已退出。

这里借鉴分层边界，不引入这些系统各自的完整状态机或资源模型。

## 三、后台服务如何复用

### 3.1 选择

| 现有服务 | 处理方式 | 理由 |
| --- | --- | --- |
| daemon 删除协调器 | 扩展为本次工作的承载点，保留唯一删除执行者 | 已有相同对象范围、遗留扫描、Notify、重试和退出管理 |
| DB retention / WAL loop | 保持独立 | 有数据库 maintenance lock 和自己的周期；不应让是否启用存储维护决定提醒是否工作 |
| runtime recovery coordinator | 继续处理异常退出及恢复 | 正常闲置退出不应被当作故障拉起；复用其恢复边界而非增加第二套恢复逻辑 |
| memory indexer | 保持独立，仅沿用既有删除索引阶段 | 索引任务不拥有 agent 生命周期 |
| parent 的正常 scheduler | 执行实际提醒输入后的 parent 工作 | 扫描器只投递内部事实，不直接调用模型 |

首期保留 `spawn_daemon_deletion_coordinator` 的启动入口。
新逻辑可放在一个聚焦的 `agent_reclamation` 模块，由该协调器调用；
模块边界用于隔离扫描、通知和退休操作，不要求立即改名整个服务。

### 3.2 调度与执行

扩展外层循环为一个受管调度点，分别处理以下工作：

1. 启动时恢复删除 job；按现有规则重建扫描进度和到期提醒。
2. 按有限批次执行历史残留扫描、清理责任扫描和已加载实例检查。
3. 投递到期提醒，仅写入正常的 durable ingress，不等待 parent 回答。
4. 推进删除执行器和闲置退出任务，分别收集结果及失败。
5. 等待 Notify、最近截止时间、任务完成或 shutdown。

已有 `drain_due_deletions` 的“持续排空”要改成公平轮转：
一个删除执行 future 顺序处理有限批次，外层 `select` 仍能响应扫描和关闭。
闲置退出采用最多一个在途的受管 future/task；所有句柄由协调器登记并等待，
不产生 detached 的删除或退出任务。删除执行者仍只有一个。
如果已有 job 的某阶段包含阻塞文件操作，应隔离该阻塞并保留执行归属；
单纯给外层加 `select` 并不能解决同步阻塞。

扫描分支不等待完整删除或 runtime join，不调用模型、不遍历 transcript，
也不在 registry 全局锁内 await。若某对象正在删除、退出或恢复，其他分支跳过，
由已取得所有权的任务完成。

shutdown 先停止接收新的 maintenance 工作，再等待或移交在途操作给 host
shutdown 路径。超时不能只丢弃 future、遗留一个无人负责的 retirement gate。
进程重启后，删除 job 从持久状态恢复；已加载实例表重建；提醒由 durable
记录去重。正常退出不得触发 recovery coordinator 立即重新加载 child。

### 3.3 两类候选来源

- **提醒候选**：从 canonical supervision、任务结果和 WorkItem 关系筛选；
  终态/工作关闭事件推进候选，周期扫描补漏。使用游标和索引，不每轮读取整棵 agent tree。
- **闲置退出候选**：只对 host 中已加载的实例取有限快照，再检查 durable 和 live blockers。
  不为扫描而加载原本未加载的 agent。

先复用已有时间/序号；不足时再增加一个轻量候选投影，包含 child incarnation、
supervision ref、有意义活动的时间/版本、提醒下次到期时间和最近事实摘要版本。
提醒投递记录应能原子建立“已调度批次 + durable ingress/outbox”，防止崩溃窗口
导致重复投递或永久漏发；实际采用直接同库事务还是现有 outbox，实施时据入口确定。
不新增复核状态机、保留期限或 parent 回答表。

只在事实变化、提醒投递或操作结果变化时写记录。状态查询、索引、提醒和扫描
不刷新活动时间；缺少历史活动证据时标记 unknown，先观察，不推定已满足清理条件。

### 3.4 提醒策略

先在 parent 正常处理子任务结果或下一轮工作时提供短清单；长期没有正常轮次时，
通过现有内部消息准入及 scheduler 发起低优先级维护提醒，服从自主工作权限和预算。
提醒必须保留 RuntimeSystem/maintenance 来源，使用合法的 agent-scope 工作归属，
不能伪造 task result、获得 operator authority 或重开已完成 WorkItem。
实现时应验证该内部 activation 的准入契约；禁止用任意 enqueue 绕过它。

初始建议：每批最多 16 个 child；每个 parent 至多一个待处理提醒；
单独唤醒提醒不超过每天一次。工作完成可以提前更新清单，不重复启动模型。
这些数值放实现配置或常量中，根据观察调整，不写入 RFC 契约。

parent 可以直接删除，也可以不处理。自然语言不被解析为删除授权；
“继续保留”不要求专门工具调用。没有状态变化时进入冷却，实际使用或删除使候选失效。
停止的 parent 不自动启动；预算耗尽保持 pending。常规提醒走内部面，只有持续失败、
失去 owner 或明显资源增长才合并通知 operator，避免每轮产生用户 brief。

## 四、具体改造与交付顺序

### P1：补齐 parent 直接删除闭环

1. 增加受限 `DeleteAgent` 内建入口，或在实施时复用已经补齐的等价入口。
   caller 从执行上下文取得，不能相信参数中的 parent ID。保持创建工具不变。
2. 在 deletion admission 增加 parent 请求类别，核对当前 supervision、ephemeral
   attachment、权限、目标 incarnation/revision，以及活跃任务、队列、等待、开放工作、
   后代责任和资源使用。删除目标引用必须携带 incarnation，避免同名重建后误删。
3. 将这些检查与 `Active -> Deleting` fence 放在同一准入事务/同步边界，
   与 invocation、message、work claim 和资源引用写入串行化。先核对各入口实际使用的锁；
   不把当前 bootstrap lock 自动视为覆盖所有竞争路径。
4. 核对 parent canonical result 已持久化。AgentHome 中仍被引用的文件要迁移到
   明确 owner 的持久位置并更新引用，或阻止删除。不要在首期顺便引入通用 artifact GC。
5. 复用删除 job、阶段和重试。工具返回 job ID、当前状态或具体 blocker。
   重复请求保持幂等，不把准入成功表述成所有资源已经回收。
6. 受保护 worktree 在实际删除阶段再次验证占用、脏状态、共享关系与引用，使用既有
   cleanup lease。现有 Quiesce/Workspace 逻辑不足的地方要补上；不能只加入口检查。

完成标志：parent 无需任何提醒或复核流程，即可安全删除自己不再需要的 child。
若保护契约无法证明，返回 blocker；不通过取消现有工作来“完成清理”。

### P2：接入后台扫描和提醒

1. 扩展现有协调器的公平调度，保留唯一删除执行者。
2. 加入候选投影、事件更新、周期补扫和 durable 提醒去重。
3. 在只读详情及正常 parent 上下文中呈现未完成清理责任，避免详情查询触发 activation。
4. 接通预算受控的内部提醒及 operator 异常汇总。
5. 对现有 retained child 只补齐责任可见性和提醒，不追溯添加到期删除规则。

完成标志：parent 忘记 child 时能重新看到清单；忽略提醒不会触发删除，
也不会造成每个扫描周期都有一次模型调用或重复审计写入。

### P3：增加安全闲置退出

1. 为 host 增加 `try_retire_idle_runtime` 路径及明确的 idle exit reason，
   不直接复用强制 `unload_runtime` 或生命周期 Stop。
2. 首期只处理完全闲置实例：无执行、queued/dequeued input、pending delivery、
   非终态任务、未退出 handle、开放工作、活跃等待/定时器/trigger、workspace occupancy
   或 bootstrap/recovery/deletion。仅保留磁盘上的工作区本身不阻止退出。
3. 捕获 incarnation、generation 和活动版本；关闭旧实例执行准入并重新验证，
   协作退出后确认主 loop 和所有 owned handle 已结束，最后移除同一 generation。
   已克隆的 RuntimeHandle 也必须遵守 gate，不能只从 registry 删除入口。
4. 新调用先取得准入则取消退休；退休先取得准入则等待旧实例退出再加载。
   已 durable 接受的输入必须继续可派发，不能落到死 handle，也不能出现双实例执行。
5. 超时保留有主的 gate 和失败证据，交既有恢复/关闭路径处理；未证明空闲时不强杀。
   限制只读 clone 生命周期，区分“主循环已退出”“registry 已移除”“内存实际释放”。
6. 初始闲置阈值建议 10 分钟，可配置/调优。涉及等待的 agent 暂不退出，
   直到 host 侧 wake 接管得到验证。开启前做真实驻留和恢复延迟测量。

完成标志：闲置 child 释放运行资源后仍能读取历史，并通过新 invocation 在同一
身份下加载新 generation；创建参数、权限和身份生命周期语义不变。

P1 可以独立交付；P2 在 P1 的可用删除入口之上闭环；P3 独立推进，
不要求为了先做闲置退出而拖延 parent 清理。特殊情况的智能巡检作为后续应用层能力。

## 五、验证及上线边界

| 范围 | 必测场景 |
| --- | --- |
| 授权 | 正确 parent；peer/伪造 parent；监督关系变更；public/independent 排除；重建后的同名 ID |
| 删除准入 | 与 invocation、排队输入、新 artifact ref 竞态的两种顺序；等待/开放工作/后代阻止清理 |
| 结果与资源 | parent 输出可读；AgentHome 文件引用保全或阻止；脏、共享、占用中的 worktree 不被移除 |
| 持久执行 | 重复删除；每阶段失败重试；准入后崩溃；phase 中断后重启；实际退出证据不被 task terminal 替代 |
| 提醒 | 无回复/自由文本无生命周期副作用；停止 parent 不唤醒；预算耗尽；重复事件和重启无重复投递；批次外对象最终有机会被检查 |
| 公平性 | 删除持续积压时提醒仍有进展；退出超时时删除仍有进展；没有全树/全 transcript 扫描与空转写入 |
| 闲置退出 | 旧 handle、新输入、config reload、recovery、shutdown 竞态；确认退出前不加载第二实例；正常退休不立即被恢复循环拉起 |

实现时先跑对应模块的最小回归，再执行 Rust PR 要求的 `cargo fmt --all -- --check`
和 `RUSTFLAGS="-D warnings" cargo check --all-targets`。针对提醒使用可控时钟和
调度预算测试；针对休眠使用真实句柄退出与恢复测试，不能只断言状态字段变化。

上线先观察候选数量、blocker 分布、扫描耗时、提醒频率和实际驻留开销；
然后启用 parent 入口与提醒，最后在竞态/恢复验证通过后启用自动闲置退出。
关闭提醒或闲置退出开关应停止新增工作，已受理的删除 job 仍按既有契约完成。

首期不保证无人授权时磁盘容量有硬上界：parent 无响应，身份和持久数据继续保留；
失去 parent 的对象按 `cleanup_required` 交授权恢复/人工处理。自动按年龄删除身份、
配额强制淘汰和远程 worker lease 均应另行设计，不加入本次范围。

## 六、与 #3430 的衔接

#3430 当前仍包含创建时暴露 `retain/delete_on_terminal` 的要求。
本计划建议把它降为可选便捷能力，优先完成 parent 直接删除、保护和后台提醒。
该取舍在设计接受后同步到 issue；本文件没有将原验收项视为已经满足或删除。
一次性任务已有的终态删除路径继续保留。

实施 PR 同步更新删除准入、监督关系与工具文档；运行体退出落地时同步 host
activation 契约。阶段状态和具体符号更新在本计划及 implementation matrix 中，
RFC 只在职责或语义边界变化时修改。

## 七、实施记录（2026-10-09）

P1、P2、P3 的代码已落在任务分支，当前继续验证，尚未上线。

| 阶段 | 已实现 | 当前验证 |
| --- | --- | --- |
| P1 | `DeleteAgent`；当前监督关系、权限和 incarnation 校验；SQLite 准入 fence；旧运行体协作退出；保守产物保护和 worktree 预检/阶段复检 | parent 授权、队列两种顺序、开放工作/文件保护、真实调用结果保全测试已通过；资源与全量回归继续执行 |
| P2 | migration 77 持久观察与 outbox；每批 16 个的 keyset 补扫；self `GetAgent` 责任清单；每天最多一批普通内部提醒；孤儿与重复失败 operator brief | 重启、幂等、停止/预算、分页与孤儿通知四组测试通过；无自由文本解析或 TTL 删除 |
| P3 | 实例 admission gate；有主的任务 handle；同 generation join/移除；host message/callback 重试定位；独立开关 | 闲置退出、读取不激活、重新加载、过期 generation 和已受理操作保护测试通过；callback 与实际进程入口测试继续执行 |

实施中发现旧 `converge_private_child_identities` 会依据缺少 home、parent 或历史 task
直接 tombstone 并移除目录。该路径已移除；明确终态的一次性子任务沿用持久删除
准入/遗留修复扫描，责任不明确的对象保留并诊断。相关启动回归改为验证保全。

提醒首期直接读取已有任务、队列、监督和状态事实，按 keyset 周期补扫，不另建
一套领域事件消费者。观察记录只在事实变化时更新；一次事务提交提醒批次、消息
outbox 和冷却时间。投递与 ack 之间重启复用同一个消息 ID，交既有队列幂等处理。
这减少事件丢失/重复投影的维护面；规模增大后可用既有事件作加速提示，周期补扫仍保留。
大 fleet 的发现延迟随批次数增加，首期不承诺固定的全量扫描完成时间。

运行体观察独立使用内存中的活动证据与单调时钟，不把扫描时间当作最后使用时间。
任务归档或缺少状态不证明空闲；缺少历史证据先等待观察期。默认 callback 是可由
host 重新解析并加载的持久入口；有实际 wait、timer 或 pending wake 的实例仍排除。

删除保护优先阻止而非迁移：有 child 产物元数据、任务输出文件、未知 AgentHome
内容、后代责任或不安全 worktree 时返回 blocker。只删除 child 自建、干净、独占、
取得 cleanup lease 的 worktree，不 force remove；Git branch 历史继续保留。
canonical parent 结果在共享 DB 中保全，身份配置与可重建缓存随明确删除一起回收。

新增配置键均支持 get/set/unset。提醒和闲置退出默认 `false`，观察阈值默认 600 秒。
部署级真实 RSS、外部进程驻留与模型恢复延迟尚未测量，因此本 PR 不自动启用、
部署或重启现网。验证数据只描述测试 runtime 的资源退出与持久事实。
