# pre-public runtime 公开前里程碑 Memo（2026-04）

日期：`2026-04-06`

这份 memo 记录当前对 `pre-public runtime` 下一阶段的判断：

- 现在的 `pre-public runtime` 处在什么阶段
- 如果目标是“先把 `pre-public runtime` 做成熟，再替换 `holon`”，下一步最该做什么
- 公开前的里程碑应该怎样切

重点不是继续写抽象架构愿景，而是把：

- 当前真实实现
- 公开前缺口
- 推荐执行顺序
- 里程碑 gate

收成一个可执行的判断。

## 一、当前阶段判断

`pre-public runtime` 已经不在“验证 runtime 能不能工作”的阶段。

它已经完成了几个关键跨越：

- long-lived、queue-centered、wake/sleep 的 runtime 语义已经成立
- coding loop 已经成立
- background task / subagent / worktree workflow 已经成立
- `run` 和 `serve` 两条执行形态都已经出现
- callback / wake / `AgentInbox` 联动已经有真实验证

因此当前主风险已经不是：

- 能力还不存在

而更接近：

- 如何把已经存在的能力收成一个可公开承诺的产品边界

一句话：

`pre-public runtime` 现在最需要的不是继续证明“它能不能做 runtime”，而是证明“它能不能成为一个成熟的 runtime product”。

## 二、为什么现在不该再以“更多功能”作为主线

当前代码和测试状态说明，很多原本路线文档里的核心工作，其实已经落地：

- runtime boundary 已经明显拆分
- prompt assembly 已经模块化
- `command_task` 已经存在
- worktree orchestration 已经存在
- `run_once` 已经存在
- callback / waiting / wake-only 机制已经存在

而且当前测试覆盖不是纸面上的：

- unit tests
- HTTP route tests
- live provider tests
- regression fixture tests
- `run_once` tests
- worktree orchestration tests

这意味着，继续把下一阶段表述成：

- “再做更多 runtime feature”

会逐步偏离真正瓶颈。

当前更高杠杆的问题是：

- 对外一句话到底是什么
- 哪条 workflow 才是公开主链路
- 哪些行为已经够稳定可以承诺
- 哪些行为仍然只能算 preview / experimental

## 三、当前最关键的公开前缺口

### 1. 产品 contract 还没有冻结

当前 `pre-public runtime` 的实现已经很多，但还没有一个足够清晰的公开表达，来明确：

- `pre-public runtime` 对外到底是什么
- `run`、`serve`、task、callback、worktree 各自是什么层级
- 哪些是核心能力，哪些只是内部支撑能力

这会直接影响：

- README
- quickstart
- benchmark narrative
- 与未来 `holon` 并回时的叙事一致性

### 2. 安全和执行边界还不够“可公开解释”

执行 substrate 已经开始出现，但当前更像：

- 架构雏形

还不完全像：

- 一个用户能理解的默认安全模型

公开前必须能回答：

- 默认允许什么
- 默认拒绝什么
- operator / external / callback / task 的权限关系是什么
- worktree 在哪些 flow 里是软约束，哪些 flow 里应该变成硬约束

### 3. 当前最强 workflow 还没被正式定义为主入口

现在最像产品的不是“抽象 runtime”本身，而是几条具体路径：

- `runtime-incubation run`
- `runtime-incubation serve`
- `AgentInbox -> pre-public runtime` 的 review-wake loop
- worktree-based reviewable coding workflow

公开前需要明确：

- 哪条是 headline workflow
- 哪条是 supporting workflow
- 哪条仍然只适合内部验证

### 4. 文档叙事开始和真实实现漂移

当前一些路线文档仍然在描述：

- runtime slicing
- prompt assembly
- command-task shape

像是“下一步重点”。

但代码层面，这些很多已经有真实落地。

如果公开前不先统一文档，会造成两种问题：

- 内部判断继续滞后于实现现状
- 对外叙事会显得像“还在讲未来”，而不是“已经有一个能跑的系统”

## 四、下一阶段不该做什么

公开前这轮，不应该把重心放在：

- 新 connector 扩张
- plugin / marketplace 叙事
- UI-first 产品面
- 复杂 remote backend
- broad sandbox backend matrix
- 新一轮大规模架构重写

这些方向未来都可能有价值。

但对当前 `pre-public runtime` 来说，它们都不是公开前的主瓶颈。

## 五、推荐的下一步主线

下一阶段建议收成一条更明确的主线：

`先收敛公开主叙事和主 workflow，再做少量直接影响公开体验的 runtime hardening。`

具体来说，最该优先做的是：

### 1. 写一份新的 public contract baseline

这份 baseline 至少要明确：

- `pre-public runtime` 的一句话定位
- `run` 和 `serve` 的关系
- callback / task / worktree 在公开产品里的位置
- 什么是 GA
- 什么是 preview
- 什么仍然只是 experimental

这一步主要是产品定义，不是实现扩张。

### 2. 冻结一条主 workflow

当前更合理的公开主链路应是：

- `local-first coding runtime`

具体形式可以是：

- `run` 作为最易上手的 one-shot 入口
- `serve` 作为长期运行的更强形态

但公开 headline 不应一上来就把所有 callback / inbox / orchestration 全部并列展开。

### 3. 做公开前 runtime hardening

当前最值得优先 harden 的，不是某几个特定 workflow mode，而是 runtime 的几类基础约束：

- result closure and continuation control
- scope preservation across follow-up and delegation
- execution boundary enforcement as profile / policy / workspace composition

这些基础约束会直接决定：

- agent 什么时候自然收口
- follow-up 是否会无意义扩 scope
- execution boundary 是否是真正的 runtime invariant
- 同一套 substrate 能否支持不止一种 workflow

### 4. 把安全模型写成可解释的默认策略

不是先追求最强 sandbox backend。

更重要的是先把默认 contract 写清楚：

- 哪些输入默认有 shell / file / task authority
- 哪些输入只能影响 planning，不能直接继承 operator 权限
- `run` 和 `serve` 各自默认如何处理 trust / control

### 5. 补一套真正面向公开的 benchmark / demo narrative

当前 benchmark 已经足够支撑内部判断，但还不够像公开材料。

公开前至少应该有：

- 一组 guardrail benchmark
- 一条可复现 demo workflow
- 一份“为什么 `pre-public runtime` 值得公开”的证据型叙事

## 六、推荐里程碑

### M-public-0：Contract Freeze

### 目标

冻结 `pre-public runtime` 的公开产品 contract。

### 应完成

- 写出新的 baseline 文档
- 明确一句话定位
- 明确 `run` / `serve` 的关系
- 明确哪些能力属于：
  - GA candidate
  - preview
  - experimental
- 清理与当前实现明显漂移的路线文档

### Done when

- 团队能稳定用同一套话解释 `pre-public runtime`
- README / roadmap / benchmark narrative 不再互相打架
- 后续实现工作开始围绕明确产品 contract，而不是抽象可能性

### 备注

这一阶段的主要成果应该是：

- 文档
- 命名判断
- 产品边界

不是新增很多功能。

### M-public-1：Preview Hardening

### 目标

把当前已经存在的 runtime 收紧成可以公开 preview 的质量。

### 应完成

- 定义 result closure contract
- 定义 continuation contract
- 定义 execution profile / policy / workspace 的组合边界
- 把 workflow-specific 约束下沉为 opt-in policy，而不是产品硬编码 mode
- 收紧默认 auth / control / callback / state-dir 行为
- 把主要 failure mode 收到可解释范围

### Done when

- `pre-public runtime` 能在公开 preview 场景下稳定收口和续跑
- follow-up / delegation 不再频繁破坏当前 objective
- execution boundary 不再主要依赖 prompt discipline
- workflow-specific safety要求可以通过 policy 选择，而不是写死在产品主 contract 里
- 默认运行行为能够被 operator 明确理解

### 备注

这一阶段的重点是：

- 质量收口
- 行为稳定性

不是继续横向扩张能力面。

### M-public-2：Workflow Proof

### 目标

把 `pre-public runtime` 最强的一条工作流做成可复现、可讲清楚、可演示的产品证据。

### 推荐主链路

优先考虑下面两条里的其中一条，不要同时把两条都当 headline：

1. `runtime-incubation run` 的 one-shot coding workflow
2. `serve + AgentInbox` 的 review-wake continuation workflow

### 应完成

- 选定公开主 workflow
- 产出 quickstart / runbook / demo
- 让 benchmark narrative 与 demo narrative 对齐
- 给出“为什么这不是另一个普通 coding CLI”的明确说明

### Done when

- 外部人可以在一个短路径里体验到 `pre-public runtime` 的独特价值
- 团队不再需要同时讲很多并列 workflow 才能解释产品
- benchmark、docs、demo 讲的是同一条主链路

### M-public-3：Holon Replacement Readiness

### 目标

让 `pre-public runtime` 达到足以承担未来 `holon` 产品位置的成熟度。

### 应完成

- `pre-public runtime` 的公开 contract 已稳定
- preview workflow 已有足够证据支撑
- `holon` / `pre-public runtime` 的边界可以开始从“双名字”收束到“单产品边界”

### Done when

- 团队可以自然地说：
  - `holon` 是对外产品名
  - `pre-public runtime` 是其现实 runtime 实现
- 替换动作不再像冒险跳跃，而像顺理成章的收束

### 备注

这不是第一步。

这是在前面几个阶段都稳定之后才应发生的产品层动作。

## 七、当前推荐执行顺序

如果现在要排一条最实际的顺序，我建议是：

1. 先做 `M-public-0`
2. 再做 `M-public-1`
3. 然后做 `M-public-2`
4. 最后才讨论 `M-public-3`

也就是说：

- 先统一产品 contract
- 再收质量
- 再做 workflow proof
- 最后才做 `holon` 替换

而不是反过来先做名字和 repo 收束。

## 八、一句话判断

当前 `pre-public runtime` 已经足够成熟，可以从“runtime buildout”阶段进入“public readiness”阶段。

下一步最重要的不是继续增加很多能力，而是：

`把已经存在的 runtime 能力收成一个可公开承诺、可演示、可替换 holon 位置的产品边界。`
