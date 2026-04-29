# pre-public runtime Substrate-First 任务拆解（2026-04）

日期：`2026-04-06`

这份文档用于替代一种过于 workflow-specific 的拆解方式。

它的目标是把下一阶段任务从：

- verification loop
- constrained repair mode
- managed worktree enforcement

这类带强场景预设的表述，

改写成更接近 runtime substrate 的表述。

## 一、为什么要改写

这些 workflow-specific 表述主要来自最近一轮 dogfooding 复盘：

- [Dogfooding retrospective（2026-04）](../notes/dogfooding-retrospective-2026-04.md)
- [Dogfooding action items（2026-04）](../notes/dogfooding-action-items-2026-04.md)

它们作为“观察记录”和“当前最痛的问题列表”是合理的。

但它们不适合直接升级成下一阶段的产品 Epic。

原因是：

- 它们过于绑定当前 supervised coding flow
- 它们容易把 `pre-public runtime` 绑成一种特定模式
- 它们描述的是症状，不是 substrate 缺口

因此更合理的做法是：

- 保留这些文档作为问题来源
- 但把下一阶段任务拆成 runtime invariant 和 control surface

## 二、推荐的任务主线

下一阶段建议围绕四个 substrate Epic 拆。

### Epic 1：Result Closure And Continuation

核心问题：

- 一次 turn / task 何时算真正完成
- 什么信号会触发继续
- 什么信号应该自然终止
- completion 和 continuation 的关系是什么

这比“tighten verification loop”更基础，因为：

- verification 只是某一类 closure signal
- future workflows 也会碰到同样问题

候选任务：

1. 定义 result closure contract
2. 定义 continuation trigger contract
3. 区分 completion、sleep、awaiting external change、awaiting task result 这几类终态

### Epic 2：Scope Preservation

核心问题：

- follow-up 到来时，当前 objective 如何被继承
- delegation 后 scope 如何回收
- 新输入是收窄旧目标、替换旧目标，还是追加目标

这比“harden constrained repair mode”更基础，因为：

- review-fix 只是 follow-up scope preservation 的一个实例
- 未来别的 continuation workflow 也会遇到同样问题

候选任务：

1. 定义 objective / delta / acceptance boundary 的运行时表达
2. 定义 follow-up handoff contract
3. 改进长期 context 下的 objective 保真度

### Epic 3：Execution Boundary Composition

核心问题：

- execution boundary 到底由什么组成
- profile、policy、workspace、worktree 各自是什么角色
- 哪些约束是默认 contract，哪些是 opt-in

这比“enforce managed worktree”更基础，因为：

- worktree 只是某种 boundary projection
- 不应直接变成所有 flow 的默认产品规则

候选任务：

1. 定义 `ExecutionProfile` v1
2. 定义 policy 叠加点
3. 定义 workflow-specific isolation 作为 opt-in policy
4. 在 selected flow 中验证 worktree constraint

### Epic 4：Public Runtime Defaults

核心问题：

- 哪些默认值会进入公开 contract
- auth / control / callback / state-dir 的默认行为如何解释
- 哪些 surface 是 GA candidate，哪些只是 preview

这部分是公开前收口，不是新功能扩张。

候选任务：

1. 冻结默认 auth / control contract
2. 冻结默认 trust contract
3. 冻结 `run` / `serve` 的公开边界
4. 用一组 guardrail benchmark 保护这些默认值

## 三、推荐初始任务列表

如果要把它落成一组更具体的 issue，推荐从下面 8 个开始：

1. 定义 result closure contract
2. 定义 continuation contract
3. 定义 objective / delta / acceptance boundary model
4. 定义 follow-up handoff contract
5. 定义 `ExecutionProfile` v1
6. 定义 workflow constraint as opt-in policy
7. 冻结 default trust / auth / control contract
8. 建立 public guardrail benchmark set

## 四、与 dogfooding 文档的关系

当前建议是：

- 保留 dogfooding retrospective / action items 不动
- 把它们视为“问题来源”和“症状记录”
- 不再把它们直接当作公开前 Epic 名字

更准确的关系应该是：

- dogfooding 文档告诉我们哪里在坏
- substrate-first 拆解告诉我们应该补哪一层

## 五、一句话判断

下一阶段不应再拆成某些具体 workflow mode 的优化项。

更合理的拆解是：

`围绕 result closure、scope preservation、execution boundary 和 public defaults 这四类 runtime substrate 继续推进。`
