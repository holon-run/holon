---
title: Task / Agent Surface Convergence
date: 2026-04-21
status: draft
---

# Task / Agent Surface Convergence

这篇 memo 只回答一个收敛问题：

- pre-public runtime 的 `Task` 抽象，是否应该收缩成“后台 shell 任务”
- `subagent_task` / `worktree_subagent_task` 是否应该继续挂在 `Task` 下面

短结论：

- 对模型暴露的 public surface，应该把 `subagent_task` 逐步从 `Task` 语义中拆出去。
- 对 runtime 内部执行层，不应该急着把 `Task` 砍成 shell-only。
- 更合理的分层是：
  - `WorkItem` 表示高层目标和进度
  - `command_task` 表示后台命令执行
  - `spawn_agent` 表示上下文隔离和有界委托
  - waiting / callback / timer 表示外部等待与唤醒

## 背景

当前 `CreateTask` 把几类不同语义的东西放在同一个入口里：

- `sleep_job`
- `subagent_task`
- `worktree_subagent_task`
- `command_task`

这会带来两个问题。

第一，模型看到的是一个统一的 `Task` 词，但这些能力并不属于同一层：

- `command_task` 更像后台执行和生命周期管理
- `subagent_task` 更像创建另一个上下文去完成一段有界工作
- `sleep_job` 更像调度 / 等待

第二，pre-public runtime 已经引入了 `WorkItem` 来表示高层持续工作。如果 `Task` 继续同时承载：

- 后台命令
- 子 agent 委托
- 定时等待

那 `Task` 会继续成为一个过宽的中间层，和 `WorkItem`、`Agent` 的边界都不够清楚。

## 当前源码和文档实际指向的方向

pre-public runtime 现有材料其实已经隐含了更清晰的三层分工。

### 1. `WorkItem` 是高层目标层

`WorkItem` 被定义为：

- 高层持续工作
- 跨 turn 存在
- 可跨多个内部 task

它回答的是：

- agent 当前在推进什么有意义的工作

而不是：

- 当前具体有什么后台执行单元正在跑

### 2. `Task` 是 runtime operational unit

现有 RFC 对 `Task` 的定义更接近：

- runtime 内部的执行单元
- 可等待
- 可取消
- 可恢复
- 可产出 `task_status` / `task_result`

这个定义本身没有错，但它不等于“public surface 里所有异步事物都应该长得像 `CreateTask(kind=...)`”。

### 3. `subagent_task` 已经在往 child agent 方向演化

现有 `agent-thread-unification.md` 已经明确把今天的 `subagent_task` 视为：

- ephemeral child agent

这其实已经说明，`subagent_task` 的本质不是“一个 task 类型”，而是：

- 一个有界生命周期的 child agent

如果继续把它留在 `CreateTask` 下面，只会让 public surface 停留在旧命名上，而不是反映真实语义。

## 关键判断

核心不是“Task 该不该存在”，而是“哪一层对模型应该暴露成 Task”。

我的判断是：

- 对模型：`Task` 应逐步收敛成“后台执行 / 可观察 job”的语义
- 对 runtime：可以保留更一般的 task/job handle 层，用来统一取消、恢复、审计和等待

也就是说：

- public surface 可以更窄
- internal execution substrate 不必同步变窄

这是因为一旦过早把内部 `Task` 定义成 shell-only，后面很容易重新长出平行抽象来承载：

- timer
- child-agent run
- interactive PTY continuation
- 其他 runtime-managed job

最后得到的不是更简单，而是两套重叠执行层。

## 推荐的 public surface 收敛

建议逐步收敛成下面四个面。

### 1. Work plane

高层工作只通过 `WorkItem` / `WorkPlan` 表达：

- 当前要交付什么
- 当前进度是什么
- 当前是否 waiting / completed

这里不直接暴露底层后台执行细节。

### 2. Command plane

后台命令统一通过：

- `exec_command`
- `TaskList`
- `TaskGet`
- `TaskOutput`
- `TaskStop`

如果后续加 `tty=true` continuation，也应继续挂在这个面下，优先采用：

- `exec_command` 启动
- 超过 `yield_time_ms` 后转成 task-owned interactive command
- 后续通过 task-oriented continuation 工具交互

这样 `Task*` 系列就有了更稳定的对象模型：

- 主要服务 `command_task`
- 少量兼容其他 runtime job

### 3. Agent plane

有界委托和上下文隔离应该显式归到 agent plane：

- `spawn_agent`
- `spawn_worktree_agent`
- 后续可能还有 `wait_agent` / `send_input` / `close_agent`

如果短期不想一次性引入完整的 Codex 风格协作协议，至少也应该先在命名和心智模型上完成迁移：

- `subagent_task` -> child agent spawn
- `worktree_subagent_task` -> worktree child agent spawn

### 4. Waiting plane

等待和唤醒应该从 `Task` 语义中进一步剥离，集中到：

- timer
- callback
- waiting intent

`sleep_job` 如果继续存在，更适合作为 runtime 内部 job 或兼容入口，而不是长期 public surface 的核心语义。

## 为什么不建议直接把 Task 改成 shell-only

如果这里说“以后 Task 就只表示后台 shell”，听起来很干净，但工程上有两个问题。

### 1. runtime 仍然需要统一的异步执行记录层

pre-public runtime 现在很多机制都依赖“可持久化的异步执行单元”：

- status
- result
- recovery
- wait policy
- cancellation
- audit trail

这些机制未必都只属于 shell。

### 2. child agent 的生命周期管理仍然需要类似 task handle 的东西

即使 public surface 改成 `spawn_agent`，runtime 内部仍然大概率需要一个可追踪记录，回答：

- 这个 child agent 是否还在运行
- 它是否已经结束
- 它的最终结果是否已回传
- 它是否需要清理

所以更合理的说法不是：

- “Task 只给 shell 用”

而是：

- “模型看到的 `Task` 应主要对应 command/background execution”
- “runtime 内部仍可保留统一 job/task handle 层”

## 推荐迁移方向

### Phase 1: 先收口 public semantics

- 保留现有 runtime 内部实现
- 弱化 `CreateTask(kind=subagent_task)` 的对外语义
- 在文档和 prompt 中明确：
  - `command_task` 是任务控制主对象
  - `subagent_task` 只是向 child agent 模型过渡的兼容形式

### Phase 2: 拆开 command plane 和 agent plane

- 新增显式 `spawn_agent` 风格接口
- 将 `subagent_task` / `worktree_subagent_task` 迁移到 agent plane
- `Task*` 系列继续主要服务 `command_task`

### Phase 3: 处理 waiting / timer

- 评估 `sleep_job` 是否继续公开
- 更偏向让 waiting / timer 进入独立 plane
- 让 `Task` 不再承担“所有异步行为”的入口职责

### Phase 4: 再考虑内部统一层是否改名

当 public surface 收敛完成之后，再决定 runtime 内部是否需要：

- 保留 `TaskRecord`
- 或改成更中性的 `JobRecord`

这个改名不该先做。

如果现在就先改内部名词，而 public surface 仍然混合：

- `command_task`
- `subagent_task`
- `sleep_job`

那只会增加重命名噪音，不会真正降低复杂度。

## 最终建议

对 pre-public runtime，更好的收敛方式是：

- 不把 `Task` 整体定义成“只给 shell 用”的 runtime 真理
- 但把 public surface 里的 `Task` 逐步收敛成“后台命令 / 可观察执行 job”
- 把 `subagent_task` 迁移到 `Agent` 语义
- 把 `sleep_job` 迁移到 waiting / timer 语义
- 让 `WorkItem`、`Task`、`Agent` 三层各自回答不同问题

最终三层分工应该是：

- `WorkItem`: 当前在做什么高层工作
- `Task`: 当前有哪些后台执行需要观察、停止、取输出
- `Agent`: 是否需要创建另一个上下文去承担一段独立工作

这比“把一切异步能力都塞进 `CreateTask`”清楚，也比“先把内部 Task 砍成 shell-only”更稳。
