# pre-public runtime Issue Drafts（Substrate-First，2026-04）

日期：`2026-04-06`

这份文档把当前 substrate-first 拆解继续落成更接近 issue 的草案。

目标不是一次性开很多大任务，而是：

- 先把下一阶段任务名字写准
- 把 workflow 症状翻译成 runtime substrate 任务
- 控制单个任务粒度

默认要求：

- 每个 issue 尽量控制在 `1-3` 天可完成
- 避免开“大而泛”的任务
- 先做 contract / invariant，再做 workflow packaging

## Epic 1：Result Closure And Continuation

### 1. RFC: result closure contract

### Why

当前“什么时候算完成、什么时候该继续”仍然混在：

- prompt
- tool result
- `Sleep`
- task result

这些行为里。

### Scope

定义 `turn`、`task`、`agent` 的终态集合和语义边界，至少覆盖：

- `completed`
- `sleeping`
- `awaiting_task_result`
- `awaiting_external_change`
- `failed`

### Done when

- 有一份文档明确说明每种终态的进入条件
- operator 可见含义是清楚的

### Depends on

- none

### 2. RFC: continuation trigger contract

### Why

continuation 触发点分散在：

- wake hint
- task result
- operator follow-up
- system tick

等路径里。

### Scope

定义：

- 哪些 trigger 只改变 liveness
- 哪些 trigger 会进入 model-visible continuation
- 哪些 trigger 会覆盖当前 objective

### Done when

- 所有 continuation source 都能映射到一张统一 trigger matrix

### Depends on

- `RFC: result closure contract`

### 3. Implement closure state model in runtime

### Why

如果 closure 只停留在文档层，`run` 和 `serve` 仍会继续各自漂。

### Scope

把 closure / awaiting / failure 语义落实到：

- runtime state
- delivery path
- operator-visible status

减少对 prompt wording 的依赖。

### Done when

- 同类结果在 `run` 和 `serve` 下表现一致
- transcript / status 能看出当前终态

### Depends on

- `RFC: result closure contract`

### 4. Add closure and continuation guardrail coverage

### Why

closure / continuation 回归很隐蔽，靠 dogfooding 不够。

### Scope

增加 fixture / integration coverage，保护：

- 应该停时停
- 应该续时续
- 外部 wake 不误触发 completion

### Done when

- 至少有一组固定 guardrail 专门保护 closure/continuation 语义

### Depends on

- `Implement closure state model in runtime`
- `RFC: continuation trigger contract`

## Epic 2：Scope Preservation

### 5. RFC: objective, delta, and acceptance boundary model

### Why

follow-up 和 repair 漂移，本质上是 runtime 没有显式表达：

- 当前目标
- 相对上轮变化
- 当前 acceptance boundary

### Scope

定义 objective、delta、acceptance boundary 的最小运行时表示。

### Done when

- 文档能说明新输入是如何收窄、替换或追加当前目标的

### Depends on

- none

### 6. RFC: follow-up handoff contract

### Why

follow-up 是：

- 普通提问
- 继续执行
- 局部修正

目前没有统一 contract。

### Scope

定义 operator follow-up 的分类和处理规则，尤其是如何继承当前 objective 和 acceptance boundary。

### Done when

- 至少能区分 `continue` / `narrow` / `replace` 三类 follow-up

### Depends on

- `RFC: objective, delta, and acceptance boundary model`

### 7. RFC: delegation handoff contract

### Why

subagent / task 能跑，但 parent 与 delegated work 之间：

- 传什么
- 不传什么
- scope 怎么回收

还不够稳定。

### Scope

定义：

- parent -> child handoff payload
- child -> parent return payload
- summary contract
- scope return 规则

### Done when

- delegation 不再主要依赖 prompt discipline 保持边界

### Depends on

- `RFC: objective, delta, and acceptance boundary model`

### 8. Implement objective fidelity support for long-lived agents

### Why

长生命周期 agent 的问题不是单纯“上下文不够”，而是当前 objective 的保真度不够。

### Scope

把 objective / delta / acceptance boundary 进入 runtime-visible context，而不是只靠 recent messages。

### Done when

- 长 follow-up 下 objective 漂移明显下降
- 有对应回归覆盖

### Depends on

- `RFC: follow-up handoff contract`
- `RFC: delegation handoff contract`

## Epic 3：Execution Boundary Composition

### 9. RFC: execution profile v1

### Why

execution boundary 现在已经有雏形，但还没成为稳定 contract。

### Scope

定义第一版 profile 集合和角色，建议先覆盖：

- `read_only`
- `workspace_write`
- `worktree_write`
- `background_worker`

### Done when

- profile 成为 agent 的显式 runtime 属性

### Depends on

- none

### 10. RFC: policy overlay contract

### Why

profile 和 policy 现在容易混成一层。

### Scope

定义：

- policy 如何在上下文上收窄 profile
- 哪些 deny 是默认行为
- 哪些 escalation 点未来可接 approval

### Done when

- 能清楚解释 “profile 是一般能力，policy 是上下文裁剪”

### Depends on

- `RFC: execution profile v1`

### 11. RFC: workspace and worktree projection contract

### Why

worktree 很容易被误当成权限主体。

### Scope

明确：

- workspace
- active root
- worktree root

各自是什么角色，强调它们是 environment projection，不是 authority owner。

### Done when

- 路径投影、execution boundary、worktree lifecycle 在文档里不再互相混淆

### Depends on

- `RFC: execution profile v1`

### 12. Implement workflow constraints as opt-in policy

### Why

某些 workflow 需要更强约束，但不应写死成产品默认模式。

### Scope

把：

- `requires managed worktree`
- 类似 narrow acceptance constraint

这类要求下沉为 opt-in policy，并选一个 flow 验证。

### Done when

- supervised coding flow 可以要求它
- `pre-public runtime` 主 contract 不被绑死

### Depends on

- `RFC: policy overlay contract`
- `RFC: workspace and worktree projection contract`

## Epic 4：Public Runtime Defaults

### 13. Freeze default trust, auth, and control contract

### Why

公开前最危险的是 README 说一套，默认行为做一套。

### Scope

冻结：

- 默认 trust mapping
- control surface 默认开放方式
- callback / remote ingress 默认安全边界

### Done when

- README
- quickstart
- runtime default config

三者一致。

### Depends on

- `RFC: policy overlay contract`

### 14. Define public guardrail benchmark set

### Why

公开前需要一个小而稳定的 benchmark 集，而不是每次跑全量矩阵。

### Scope

从现有 benchmark / regression 中抽出 public-facing guardrail，覆盖：

- closure
- scope preservation
- coding convergence
- follow-up correctness

### Done when

- 每次 public contract 相关改动都有固定 benchmark rail

### Depends on

- `Add closure and continuation guardrail coverage`

### 15. Publish run-first quickstart workflow

### Why

`runtime-incubation run` 应该是公开第一入口，而不是所有 primitive 并列暴露。

### Scope

写出最短 quickstart 和 demo task，让新用户一页文档就能跑通。

### Done when

- 有一条清晰路径完成本地 coding 或 analysis task
- 并且能看到明确结果

### Depends on

- `Freeze default trust, auth, and control contract`

### 16. Define serve as continuation workflow, not a second product

### Why

`serve` 很强，但如果讲法不对，会变成另一个产品边界。

### Scope

把 `serve` 收口成长期运行模式，附一个最小 continuation demo，优先：

- wake-only
- task continuation

### Done when

- 能一句话解释 `serve`
- 不需要先讲很多 control surface 细节才能让人理解

### Depends on

- `RFC: continuation trigger contract`
- `Freeze default trust, auth, and control contract`

## 推荐先开哪 5 个

如果只开最近两周最该做的 5 个，我建议是：

1. `RFC: result closure contract`
2. `RFC: continuation trigger contract`
3. `RFC: objective, delta, and acceptance boundary model`
4. `RFC: execution profile v1`
5. `Freeze default trust, auth, and control contract`

原因是：

- 这 5 个最接近下一阶段的骨架
- 它们决定后面的 runtime hardening 是不是建立在稳固 contract 上
- 它们不会把团队重新拖回 workflow-specific 修补

## 一句话判断

当前最该开的任务不是“某个 mode 的优化”，而是：

`先把 runtime invariant、execution boundary 和 public defaults 这三层 contract 定住。`
