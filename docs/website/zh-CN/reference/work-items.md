---
title: 工作项
summary: 工作项字段、状态、生命周期操作，以及操作它们的 CLI 和 HTTP 接口。
order: 38
---

# 工作项

> **心智模型与规格契约：** 理解工作项的设计初衷与核心对象关系，请查阅[运行时模型概念](/zh-CN/concepts/runtime-model.md)；查阅状态机流转与调度器内部契约，请参阅维护者专属的[工作项规格](/zh-CN/spec/work-items.md)。

工作项是 Holon 跟踪工作时使用的持久单元。当一个目标需要自己的生命周期、进度跟踪或跨轮次连续性时，就使用工作项。

## 什么时候使用工作项

以下情况应创建或更新工作项：

- 跨越多个轮次
- 需要可恢复的进度
- 等待外部状态，例如 CI、回调或操作者输入
- 有明确的验收标准，需要显式跟踪

以下情况不要创建工作项：

- 随口一问的问题
- 一次性的解释
- 短暂的检查
- 能立刻做完的轻量当前轮次任务

## 工作项生命周期

工作项的状态包括：

- **open** — 仍然处于活跃状态
- **completing** — 已请求标记完成，但尚未完成必需的报告绑定
- **completed** — 已显式完成
- **current** — Agent 当前的工作焦点
- **queued** — 当前焦点之外、可供调度器选中的已开启项
- **yielded** — 在工作项延续流转中暂时让出执行权的挂起项
- **blocked** — 正在等待某个具体阻塞项
- **waiting for operator** — 需要操作者澄清或审批
- **runnable** — 就绪可执行

关键区别在于：

- **生命周期（state）**说明目标是 open、completing 还是 completed
- **焦点（focus）**说明该项是否是当前活动项
- **就绪状态（readiness）**说明调度器是否应当恢复它

## 工作项存了什么

每个工作项包含：

- **objective** — 目标的简短陈述
- **plan artifact** — 描述预期做法的持久 Markdown 计划文件
- **plan artifact status** — 计划产物解析状态：`available`（就绪）、`missing`（元数据已记录但文件缺失）、`unreadable`（不可读）或 `not_recorded`（未记录产物）
- **plan status** — `draft`、`ready` 或 `needs_input`
- **todo list** — 有实质意义的进度步骤清单
- **blocked by** — 进度无法继续时的具体阻塞项
- **recheck deadline** — 重新评估阻塞项的回退超时时限

把工作项当作协作与协调状态，而不是临时的草稿纸。

### 记录字段（Record fields）

持久化的 `WorkItemRecord` 包含以下字段。读取模型（Read model）的投影视图可能会额外包含焦点、就绪状态、调度状态与计划文件解析状态，这些投影字段不应与持久存储的生命周期 `state` 混淆。

| 字段 | 契约约束 |
|---|---|
| `id`、`agent_id`、`workspace_id` | 工作项标识、所属 Agent 与工作区 |
| `revision` | 用于拒绝陈旧状态迁移的版本号 |
| `objective` | 被跟踪的目标 |
| `state` | `open`、`completing` 或 `completed` |
| `plan_status` | `draft`、`ready` 或 `needs_input` |
| `plan_artifact` | 可选的计划描述符，包含所属 Agent/工作区标识、相对与绝对路径、哈希值、字节数、时间戳、预览文本及预览完整性标记 |
| `todo_list` | 包含 `text` 与 `state`（`pending`、`in_progress`、`completed`）的清单条目数组 |
| `work_refs` | 证据引用数组，包含类型、引用标识、记录原因、状态、时间戳及可选的标题/来源/元数据 |
| `blocked_by` | 当前阻塞说明（如有） |
| `blocked_by_wait_id` | 建立该阻塞的等待标识；独立阻塞或旧版阻塞此项为空 |
| `recheck_at`、`recheck_consumed_at` | 回退重新核验的截止时间戳及其被消费时间戳 |
| `result_brief_id`、`result_summary` | 正式完成报告的关联标识与摘要文本 |
| `completion_intent` | 完成意图来源与版本、必需的报告状态以及报告关联 |
| `created_at`、`updated_at`、`turn_id` | 记录时间戳与可选的关联交互轮次 |

`plan_artifact_status=missing` 或 `unreadable` 并不会使工作项本身不可查询。请始终读取运行时返回的真实计划路径，不要凭工作项 ID 主观拼接路径。

## 核心操作

典型的工作项操作：

- `CreateWorkItem` — 创建一个新的跟踪目标
- `PickWorkItem` — 让一个 open 项成为当前焦点
- `UpdateWorkItem` — 修改目标、计划状态或 todo 清单
- `ListWorkItems` — 查看 current、open、blocked 或 completed 的工作
- `GetWorkItem` — 详细查看一个工作项
- `CompleteWorkItem` — 把目标标记为完成
- `WaitFor` — 显式登记等待条件，且不改变任务的归属权

修改计划正文请直接编辑计划文件。对于 Agent 的执行等待，应调用 `WaitFor`，不要通过 `UpdateWorkItem` 修改阻塞字段。操作者的 HTTP PATCH 接口具有独立的 `blocked_by`/`recheck_after` 控制字段，详见 [HTTP 控制平面参考](http-control-plane.md)；这些不是 Agent 工具的入参。

`PickWorkItem` 可以通过延续机制将当前执行让渡给另一个工作项。设置 `clear_blocker=true` 时必须提供阻塞已解决的事实依据和明确原因，不能用它来绕过让渡项的正常返回路径。

`CompleteWorkItem` 要求齐备的验收证据以及非空的操作者完成报告。绑定执行的完成会一并结算当次执行；脱离绑定的完成在允许时保留当前执行。正在其他执行中处于活跃状态的工作项，无法被不同的执行或外部控制请求标记完成。操作者的 HTTP 完成接口需要传入 `report_text` 并将其持久化为权威报告。

## 工作流示例

1. 先看这个目标是否已有 open 的工作项
2. 只有目标有自己的生命周期时才创建新工作项
3. 验收边界清楚后，编辑持久计划文件
4. 有实质进展后更新 todo 清单
5. 显式记录阻塞项，而不是悄悄扩大范围
6. 只有验收证据齐备后才完成该项

## 计划状态

有意识地使用计划状态：

- **`draft`** — 目标已存在，但做法还在成形
- **`ready`** — 计划已足够稳定，可以执行
- **`needs_input`** — 下一步取决于操作者输入

这很重要，因为运行时能区分活跃可运行的工作和应该暂停的工作。

## 调度器就绪模型

工作项就绪状态是调度器的输入。open 且 runnable 的工作项可以由调度器恢复或响应系统 tick，而 blocked 或 waiting 的项应暂停，直到解除条件发生变化。

结束一轮只会让 Agent 休息。它不会把当前工作项标记为 blocked、waiting 或不可运行。如果暂时无法推进，就调用 `WaitFor`：

- 需要操作者输入时用 `wake=operator_input`
- 等待任务时用 `wake=task_result`，并带上 `resource=<task_id>`
- 等待外部系统时用 `wake=external`，并带上 `resource=<external object>`，例如 PR、CI 运行、URL 或持久 inbox 来源
- 使用 `wake=timer` 并传入运行时返回的真实 `timer_id`
- 使用 `wake=system` 等待下一次非工作队列系统唤醒
- 外部系统可以主动唤醒 Agent 时，用外部触发器

只有当调度器下次恢复能取得有用进展时，才让工作项保持 runnable。这样可以避免 Agent 在休眠的同时还留着一个 open 项，被系统反复 tick。

## todo 清单的最佳实践

好的 todo 项：

- 以结果为中心
- 能跨轮次存续
- 在真实进展之后更新

避免：

- 记录每条细碎的 shell 命令
- 把 todo 清单当临时笔记
- 计划变化后仍留着过时的清单

## 阻塞与等待

工作项无法继续时：

- 调用 `WaitFor`，并给出具体的 `reason`
- 需要操作者澄清时用 `wake=operator_input`
- 具体的任务或外部等待用 `wake=task_result` 或 `wake=external`
- 只有工作确实跨轮次时，才挂上外部等待机制

每次调用 `WaitFor` 都必须选择 `delivery=final`（需要报告）或 `silent`（静默）。若让出执行权的 final 模式需要输出面向操作者的报告，应遵循返回的 `continuation`：`continue_turn` 代表立即在当前轮次继续工作；`yield_and_wait` 代表挂起等待唤醒；`yield_and_reenter` 代表等待排队结果重新进入模型。已被消费的结果不会重复注册等待。

存在绑定的执行工作项时，该工作项享有等待所属权；传入的显式冲突项会被忽略。若无绑定，解析顺序依次为：显式指定的所属已开启项、轮次绑定项、当前焦点或 Agent 生命周期级。即使存在独立的业务阻塞，匹配成功的唤醒信号依然能够恢复核验工作，但这不会清除该业务阻塞，也不会自动使常规工作项变为 runnable。

关于回调与定时器契约，请参阅[事件与定时器](events-and-timers.md)。

## 与任务的关系

工作项和任务不一样：

| 面 | 用途 |
|---|---|
| 工作项 | 跟踪目标和进度 |
| 任务 | 表示正在运行的执行，例如命令或子 Agent |

一个工作项可能随时间创建多个任务。任务是执行；工作项是意图和进度。

任务始终保留创建时绑定的工作项所有者。切换焦点、创建新项或从其他项发起等待，绝不会迁移任务的所有权。`WaitFor(task_result)` 可以等待同一 Agent 任意工作项下的任务；属于其他 Agent 的任务会被拒绝。

### 委托任务快照与 Token 消耗

`InvokeAgent(new_subagent)` 返回带有执行结果的受监督子 Agent 句柄。`InvokeAgent(existing_agent)` 返回一个在对等方首次发送后续持久消息时即解除等待的句柄；这不构成业务完成的承诺。多个并发等待可能会同时观察到同一条对等方回复。

子 Agent 的任务状态与输出快照能够汇报累积的 `token_usage`：

```json
{
  "total": {
    "input_tokens": 12450,
    "output_tokens": 3840,
    "total_tokens": 16290
  },
  "total_model_rounds": 5,
  "last_turn": {
    "input_tokens": 2100,
    "output_tokens": 720,
    "total_tokens": 2820
  }
}
```

| 字段 | 含义 |
|---|---|
| `total` | 子 Agent 跨轮次的累积 Token 计数 |
| `total_model_rounds` | 子 Agent 发起的模型往返调用总轮次 |
| `last_turn` | 最近一轮调用的 Token 计数（可用时提供） |

任务状态还提供受支持的后续交互信息。命令任务输入需要确认支持交互输入；受监督子 Agent 后续跟进需依据 `child_supervision.followup_target`。终态句柄无法通过 `TaskInput` 重启。

子 Agent 在调用完成后会保留下来供后续复用。父 Agent 负责子 Agent 的生命周期清理，在删除前必须核查其当前的代次（incarnation）与阻塞原因。父 Agent 的 `GetAgent` 摘要最多暴露 16 个 `subagent_cleanup` 条目。活跃的执行状态、未清理的后代以及脏的或共享的 worktree 都会阻止删除。如果父 Agent 已经判定相关记录不再需要，未关闭的工作项或已结束的任务本身不会阻止清理。可选的 `reminders_enabled` 提醒机制与 `idle_retirement_enabled` 运行时退役机制不会删除持久化的子 Agent 身份。

## 与 Agent 的关系

Agent 可以在多个工作项之间切换焦点，但通常同一时间只维护一个当前跟踪目标。如果目标发生了实质变化，先更新或切换工作项，再做高承诺的工作。

## 常见错误

- 每个小问题都新建一个工作项
- 范围变了却不更新计划
- 工作项一直开着却不记录阻塞项
- 没有验证证据就完成工作项

## 另见

- [运行时模型](/zh-CN/concepts/runtime-model.md) — 运行时生命周期中的工作项
- [运行长生命周期任务](../guides/run-long-lived-task.md) — 跟踪目标直至验收
- [多 Agent 协作](/zh-CN/guides/delegate-work.md) — 委派工作和任务监督
