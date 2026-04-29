# pre-public runtime Public Contract Baseline（2026-04）

日期：`2026-04-06`

这份文档是 `pre-public runtime` 在公开前阶段的第一份 product contract baseline。

它的目的不是记录所有实现细节，而是明确：

- `pre-public runtime` 对外到底是什么
- 当前公开主入口是什么
- `run` 和 `serve` 分别代表什么
- 哪些能力可以进入公开承诺
- 哪些能力仍然只适合 preview / experimental
- 默认的 trust / execution 边界应该如何解释

这份 baseline 应作为：

- README 收口参考
- quickstart 收口参考
- benchmark narrative 收口参考
- 未来并回 `holon` 时的产品边界参考

## 一句话定位

`pre-public runtime` 是一个 local-first、headless、event-driven 的 agent runtime，适合让 agent 在本地工作区中持续执行、等待外部变化，并在需要时恢复任务。

更短一点：

`pre-public runtime` 是一个让 agent 在本地优先环境里持续工作的 runtime。

## 一、产品边界

`pre-public runtime` 的产品边界应明确为：

- runtime
- control plane
- local workspace execution
- task orchestration
- event-driven wake / sleep continuity

`pre-public runtime` 不应对外被表述成：

- chat UI
- all-in-one agent platform
- connector marketplace
- remote agent hosting platform
- heavy integration hub

它解决的核心问题不是“和模型对话”，而是：

`如何让 agent 在本地优先环境里跨时间持续工作，同时不丢掉执行边界、任务状态和信任边界。`

## 二、当前公开主入口

当前 baseline 应明确只保留两个公开入口层级：

### 1. `runtime-incubation run`

这是最简单、最适合公开体验的入口。

它代表：

- one-shot local execution
- 明确输入
- 明确输出
- 可以直接用于分析、修复、验证一类任务

对外应把它讲成：

- `pre-public runtime` 最容易上手的默认入口

### 2. `runtime-incubation serve`

这是更强但更重的入口。

它代表：

- long-lived agent runtime
- queue / wake / sleep continuity
- callback / timer / webhook / control surfaces
- 为外部事件驱动的持续工作提供运行时承载

对外应把它讲成：

- `pre-public runtime` 的长期运行模式

### 不应作为 headline 的入口

下面这些当前不应单独成为公开 headline：

- `prompt`
- `task`
- `timer`
- `control`
- `dump-prompt`

这些更适合被解释成：

- runtime operator surface
- debugging / orchestration / control primitives

而不是第一层产品入口。

## 三、`run` 与 `serve` 的关系

这是当前 contract 里最需要讲清楚的地方。

推荐表达是：

- `run` 是最轻量的执行模式
- `serve` 是长期运行的持续模式
- 两者属于同一个 runtime product，而不是两个不同产品

更具体地说：

- `run`：适合 one-shot、局部任务、快速验证
- `serve`：适合等待事件、被唤醒、跨时间续跑、和外部系统形成闭环

不建议对外讲成：

- `run` 是稳定内核
- `serve` 是另一个实验性产品

更顺的表达应是：

- `run` 和 `serve` 是同一产品下的两种运行形态

## 四、当前推荐 headline workflow

公开前 baseline 应只选一条 headline workflow。

当前更合理的选择是：

`local-first coding runtime`

也就是：

- agent 在本地工作区里读文件、改文件、跑命令、做验证
- 可以在短任务里一次完成
- 也可以在长期模式里持续跟进外部变化

这个 headline workflow 下，当前更适合的公开体验顺序是：

1. 先体验 `runtime-incubation run`
2. 再理解 `runtime-incubation serve`
3. 最后再理解 callback / `AgentInbox` / wake-only continuation

而不是反过来先从长期事件编排讲起。

## 五、GA / Preview / Experimental 分层

当前 baseline 建议按下面方式分层。

### GA candidate

这些能力已经足够接近可公开承诺的稳定面：

- `runtime-incubation run`
- 本地 workspace file tools
- 本地 shell execution
- analysis / coding 两类基础 prompt mode
- task output / result shaping
- 基本 transcript / status / tail inspection

注意：

这里的 “GA candidate” 不是说今天就发布 GA。

它表示：

- 这些能力已经足够接近未来公开主 contract
- 后续不应再频繁改名或重构产品含义

### Preview

这些能力已经有真实实现，值得公开试用，但还不应给过强承诺：

- `runtime-incubation serve`
- callback capability
- timer / webhook / remote ingress
- background task orchestration
- `subagent_task`
- `worktree_subagent_task`
- managed worktree workflow
- `AgentInbox` continuation loop

这些能力的共同特点是：

- 有真实价值
- 有真实实现
- 但 operator protocol、默认约束、failure mode 仍在继续收口

### Experimental

这些能力当前更适合作为内部或高级用户能力，不应成为第一层公开承诺：

- 大规模并行 worktree orchestration 叙事
- future remote backend / stronger sandbox backend
- 更复杂的 condition waiting / subscription runtime model
- 多 provider 叙事扩张
- benchmark framework 作为公开卖点本身

## 六、默认 trust / execution contract

公开前必须能把默认边界讲清楚。

当前推荐公开解释如下：

### 1. `pre-public runtime` 不是“默认无限权限 agent”

`pre-public runtime` 会区分输入来源和信任级别。

至少应该让用户理解：

- operator 输入
- system/runtime 输入
- callback / webhook / external 输入

它们不是同一回事。

### 2. 外部输入默认不应继承 operator authority

当前产品 contract 应明确：

- 外部事件可以影响 wake、planning、continuation
- 但不应默认获得和 operator 一样的 shell / task / control 权限

这件事不是实现细节，而是产品承诺的一部分。

### 3. execution boundary 应解释成 profile + workspace，而不是命令审批

`pre-public runtime` 当前更合理的公开解释不是：

- 每条命令都弹审批

而是：

- agent 有自己的 execution boundary
- task 可以收窄它
- workspace / worktree 负责把它投影到具体工作区

也就是说：

`pre-public runtime` 更接近长期运行 agent 的 execution profile 模型，而不是交互式逐命令审批模型。

### 4. worktree 是 workflow safety 边界的重要组成

当前 baseline 应明确：

- worktree 不只是实现细节
- 它是受监督 coding flow 的重要安全和 review 边界

公开前下一步应继续推进的方向是：

- 在选定 workflow 里把 worktree 从“推荐做法”变成“可 enforce 的运行时约束”

## 七、与 `AgentInbox` 的关系

当前公开 contract 不应把 `AgentInbox` 混进 `pre-public runtime` 本体里。

更清楚的表达应该是：

- `pre-public runtime` 负责 runtime meaning
- `AgentInbox` 负责 source hosting / activation / delivery

对外可以讲：

- `pre-public runtime` 可以与 `AgentInbox` 组合，形成 event-driven continuation workflow

但不应讲成：

- `pre-public runtime` 本身就是 connector / inbox / subscription hub

## 八、当前非目标

公开前 baseline 应继续坚持这些 non-goals：

- 不做 chat-first 产品
- 不做 GUI-first workflow shell
- 不做 generic bot marketplace
- 不做 full VM / container sandbox product
- 不做 provider-heavy connector hub
- 不做 “everything agent platform”

## 九、当前最应该对外证明的能力

如果只能对外证明一件事，当前最值得证明的不是：

- `pre-public runtime` 支持多少工具

而是：

`pre-public runtime` 能让 agent 在本地工作区中持续推进任务，而不是只完成一次 prompt-response。

具体应优先证明的场景：

- one-shot local coding task
- 任务完成后的验证与明确结果
- 在长期模式里被事件唤醒后继续推进同一个任务

## 十、公开前的文档收口要求

基于这份 baseline，后续 README / quickstart / roadmap 至少要保持下面几件事一致：

- 一句话定位一致
- `run` / `serve` 关系一致
- headline workflow 一致
- GA / preview / experimental 分层一致
- `AgentInbox` 与 `pre-public runtime` 的分层一致
- trust / execution 默认解释一致

如果这些文档继续各讲各的，就说明 public contract 还没有真正冻结。

## 十一、一句话判断

当前 `pre-public runtime` 的公开 contract 应收敛成：

`一个让 agent 在本地优先环境里持续工作的 runtime product。`

它的默认公开入口应是：

- `run` 先行
- `serve` 作为更强形态跟上

它的默认产品叙事应强调：

- local-first
- long-lived
- event-driven
- execution boundary
- reviewable coding workflow

而不是把所有 runtime primitive 同时抬成并列 headline。
