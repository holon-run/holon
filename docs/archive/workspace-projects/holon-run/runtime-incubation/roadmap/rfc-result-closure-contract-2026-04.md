# RFC: Result Closure Contract（2026-04）

日期：`2026-04-06`

关联 issue：

- `holon-run/runtime-incubation#44` `RFC: result closure contract`

这份 RFC 试图回答一个基础问题：

`pre-public runtime` 在一次 turn、一次 task、以及一个长期运行 agent 上，怎样判断“当前工作已经闭合”，以及闭合之后系统应该进入什么状态。

重点不是定义某个具体 workflow 的提示词，而是定义 runtime contract。

## 一、问题

当前 `pre-public runtime` 的“何时完成、何时继续、何时等待”语义仍然分散在多个地方：

- prompt wording
- text-only assistant round
- `Sleep`
- task creation / task result
- callback / waiting intent
- timer
- operator follow-up

这会带来几个问题：

- 同一种运行现象在 `run` 和 `serve` 中可能被不同方式解释
- “完成” 和 “等待未来变化” 之间的边界不够稳定
- operator 很难从 status / transcript 上理解 agent 当前到底是在等什么
- workflow-specific 症状会被误当成产品模式问题，而不是 runtime closure 问题

因此需要一份统一的 result closure contract。

## 二、目标

这份 RFC 只回答两个问题：

1. 当前 unit of work 是否已经闭合
2. 闭合之后是终止，还是进入等待态

这里的 unit of work 至少覆盖三层：

- `turn`
- `task`
- `agent`

## 三、非目标

这份 RFC 不直接定义：

- 具体 prompt wording
- verification 策略
- review-fix workflow
- worktree policy 细节
- approval UX

这些都会依赖 closure contract，但不应反过来定义它。

## 四、核心判断

### 1. closure outcome 必须由 runtime 最终判定

`agent` 可以提供 intent hint，但不能成为 closure outcome 的最终真相来源。

原因是：

- agent 可能声称“已完成”，但 runtime 仍看到活跃 task、pending wait、未闭合 objective
- agent 可能声称“在等外部变化”，但 runtime 看到的其实是“正在等 operator 决策”

因此：

- runtime 是 closure outcome 的 source of truth
- agent 只能提供 closure intent hint

### 2. `sleeping` 不应作为语义 closure outcome

`sleeping` 更适合作为一种 runtime posture：

- 当前没有执行回合在运行
- runtime 已经挂起，等待未来 trigger

但它不应直接等于某种业务语义。

因为 agent 在下面这些情况下都可能处于 sleeping posture：

- awaiting operator input
- awaiting external change
- awaiting task result
- awaiting timer

所以需要区分：

- 语义层：为什么在等
- runtime posture：当前是不是 suspended

### 3. `awaiting_operator_input` 必须单独存在

它不能被合并进：

- `awaiting_external_change`
- 或笼统的 `waiting`

原因是：

- `awaiting_operator_input` 表示当前 objective 已经走到需要人类明确输入或决策的点
- 这时外部事件不应自动替代 operator 输入
- 它在 trust boundary 和 continuation policy 上与外部等待完全不同

## 五、模型

推荐使用两层模型。

## 5.1 Closure Outcome

closure outcome 先只保留三类：

- `completed`
- `failed`
- `waiting`

解释：

- `completed`
  - 当前 unit 已闭合
  - 当前 unit 不再等待自身的进一步结果
- `failed`
  - 当前 unit 以明确失败闭合
  - 需要显式恢复、重试或新输入
- `waiting`
  - 当前 unit 已闭合当前回合，但明确在等待未来 trigger
  - 这不是失败，也不是 generic completion

## 5.2 Waiting Reason

当 `closure_outcome = waiting` 时，再细分等待原因：

- `awaiting_operator_input`
- `awaiting_external_change`
- `awaiting_task_result`
- `awaiting_timer`

解释：

- `awaiting_operator_input`
  - 当前 objective 需要人类决策、澄清、批准或补充信息
- `awaiting_external_change`
  - 当前 objective 仍由 runtime 持有，但必须等待外部世界的新信号
- `awaiting_task_result`
  - 当前 objective 的后续推进已经交给 task / child execution
- `awaiting_timer`
  - 当前 objective 需要在时间条件满足后再继续

## 六、Runtime Posture

closure outcome 与 runtime posture 分离。

当前推荐只保留下面这个判断：

- `sleeping` 是 runtime posture，不是 closure outcome

也就是说，一个 agent 可能处于：

- `waiting + awaiting_external_change + sleeping`
- `waiting + awaiting_task_result + sleeping`
- `waiting + awaiting_operator_input + sleeping`

但它们在语义上完全不同。

## 七、来源与职责

## 7.1 Runtime 是最终裁决者

runtime 需要依据可观察事实生成 closure record。

可观察事实包括：

- 是否发生 runtime/provider error
- 是否有明确最终文本
- 是否调用了 `Sleep`
- 是否创建了 task
- 是否存在活跃 waiting intent / callback
- 是否存在活跃 timer
- 是否存在显式 operator follow-up requirement
- 当前 objective 是否已经闭合

## 7.2 Agent 只能提供 intent hint

agent 可以通过以下方式提供 hint：

- `Sleep(reason=...)`
- task creation
- callback / waiting registration
- future dedicated wait primitive

但这些 hint 不直接等于最终 outcome。

runtime 仍然要根据系统事实做最终归并。

## 八、Outcome Derivation

runtime 在 turn 结束时，应该生成一份 closure record。

概念结构示意：

```json
{
  "closure_outcome": "waiting",
  "waiting_reason": "awaiting_external_change",
  "runtime_posture": "sleeping",
  "evidence": [
    "sleep_called",
    "active_waiting_intent_exists",
    "no_pending_operator_requirement"
  ]
}
```

这不是要求今天就用这个 JSON 结构落盘。

它表达的是：

- runtime 不只给结果
- runtime 还应保留判定依据

这样 operator surfaces 和调试工具才能解释当前状态。

## 九、推荐判定顺序

第一版建议使用“显式 signal 优先、runtime facts 校验”的顺序。

### 1. 先看是否失败

如果存在：

- runtime error
- provider error
- 明确不可恢复的 terminal failure

则：

- `closure_outcome = failed`

### 2. 再看是否明确等待 operator

如果当前 turn 已经走到必须等待 operator 输入的点，则：

- `closure_outcome = waiting`
- `waiting_reason = awaiting_operator_input`

这类情况的关键特征应是：

- 没有足够信息继续
- 需要人类澄清或决策
- 不应由普通 external trigger 自动替代

### 3. 再看是否等待 task result

如果当前 objective 的下一步进展已经显式委托给 task / child execution，则：

- `closure_outcome = waiting`
- `waiting_reason = awaiting_task_result`

### 4. 再看是否等待外部变化

如果存在：

- active waiting intent
- callback registration
- external watch contract

则：

- `closure_outcome = waiting`
- `waiting_reason = awaiting_external_change`

### 5. 再看是否等待 timer

如果当前状态明确依赖时间条件，则：

- `closure_outcome = waiting`
- `waiting_reason = awaiting_timer`

### 6. 否则视为完成

如果没有等待条件，也没有失败条件，并且当前 objective 已闭合，则：

- `closure_outcome = completed`

## 十、Turn / Task / Agent 三层含义

## 10.1 Turn

turn 的 closure 表示：

- 这一轮 agent 执行已经闭合

它不等于：

- 整个 task 已结束
- 整个 agent 生命周期结束

turn 完成后，runtime 可以进入：

- completed turn
- waiting turn
- failed turn

## 10.2 Task

task 的 closure 表示：

- 一个 bounded delegated unit 已闭合

task 完成通常会变成：

- `task_result`
- `task_status`

再重新进入 parent agent 的 continuation path。

## 10.3 Agent

agent 的 closure 不表示 agent 生命周期终止。

对长期运行 agent 来说，更准确的理解是：

- agent 的当前活动回合已闭合
- agent 当前处于 waiting / completed / failed 的某种可见状态

因此 agent 层需要特别注意：

- closure outcome
- waiting reason
- runtime posture

三者不能混成一个字段。

## 十一、Invariants

第一版建议至少保持下面这些 invariant：

1. 一个 unit 在一次 closure decision 后只能落到一种 `closure_outcome`
2. `completed` 和 `failed` 不能同时成立
3. `waiting` 不是失败，也不是 generic completion
4. `waiting_reason` 只有在 `closure_outcome = waiting` 时才有意义
5. `sleeping` 不是 closure outcome，只是 runtime posture
6. closure result 必须能被 operator-visible surfaces 解释

## 十二、对当前实现的影响

这份 RFC 会直接影响下面几层：

- `run` final status mapping
- `serve` wake / sleep mapping
- task/result rejoin behavior
- delivery derivation
- status / transcript surfaces

但第一阶段不要求立刻重写所有实现。

更合理的顺序是：

1. 先冻结 contract
2. 再决定 runtime state 如何映射
3. 再补 guardrail coverage

## 十三、开放问题

当前仍值得继续讨论的问题：

1. `awaiting_manual_resume` 是否需要单独成为 waiting reason
2. `awaiting_operator_input` 是否应要求显式 runtime signal，而不是靠 prompt inference
3. task delegation 后 parent 是否总应进入 `awaiting_task_result`，还是某些场景可继续前进
4. 是否需要未来增加显式 agent-facing primitive，例如：
   - `Complete`
   - `WaitForOperator`
   - `WaitForExternalChange`

## 十四、Decision

当前建议的方向是：

- `pre-public runtime` 用 `closure_outcome` 区分完成、失败与等待
- 用 `waiting_reason` 区分等待的语义原因
- 用 `sleeping` 表示 runtime posture，而不是业务语义
- 最终 outcome 必须由 runtime 判定，agent 只能提供 intent hint
