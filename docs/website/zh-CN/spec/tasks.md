---
title: 任务
summary: 当前任务生命周期、终态重入，以及命令/子 Agent 监督契约。
order: 50
---

# 任务

本页定义受管任务执行的当前契约：生命周期、终态重入和监督接口。

> **Last verified:** 2026-05-25 against `src/types.rs` `TaskRecord`,
> `TaskStatus`, `TaskKind`, `TaskHandle`, `TaskWaitPolicy`, and the tool
> implementations in `src/tool/tools/{exec_command,task_list,task_status,
> task_input,task_stop,invoke_agent}.rs`.

输出观测契约已按 #3534 M2 更新，见
[Agent 输出观测](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-output-observation.md)。

## 源 RFC

- [命令工具家族](https://github.com/holon-run/holon/blob/main/docs/rfcs/command-tool-family.md)
- [Task 接口收窄](https://github.com/holon-run/holon/blob/main/docs/rfcs/task-surface-narrowing.md)
- [交互式命令延续](https://github.com/holon-run/holon/blob/main/docs/rfcs/interactive-command-continuation.md)
- [Agent 委派工具平面](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-delegation-tool-plane.md)
- [Agent 控制平面模型](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-control-plane-model.md)
- [运行时调度器契约](https://github.com/holon-run/holon/blob/main/docs/rfcs/runtime-scheduler-contract.md)

## 任务类型

| `TaskKind` | 说明 |
|------------|-------------|
| `CommandTask` | 通过 `ExecCommand` 执行 shell 命令 |
| `ChildAgentTask` | 父级监督的子 Agent 任务 |
| `ActorInvocation` | 由 `InvokeAgent` 创建的规范 Agent 调用任务 |
| `SleepJob` | 内部休眠定时器（模型不可见） |
| `SubagentTask` | 遗留的子 Agent 类型（迁移到 `ChildAgentTask`） |
| `WorktreeSubagentTask` | 遗留的 worktree 隔离子 Agent（迁移中） |

## 任务生命周期

```text
              ExecCommand / InvokeAgent
                       │
                       ▼
                 ┌──────────┐
                 │  Queued  │
                 └────┬─────┘
                      │
                      ▼
                 ┌──────────┐     TaskStop
                 │ Running  │──────────────┐
                 └────┬─────┘              │
                      │                    ▼
          ┌───────────┼───────────┐  ┌────────────┐
          ▼           ▼           ▼  │ Cancelling │
    ┌──────────┐ ┌──────────┐ ┌────┴─┴─────┐      │
    │Completed │ │  Failed  │ │Interrupted│      ▼
    └──────────┘ └──────────┘ └───────────┘┌──────────┐
                                           │Cancelled │
                                           └──────────┘
```

**终态：** `Completed`、`Failed`、`Cancelled`、`Interrupted`。
**非终态：** `Queued`、`Running`、`Cancelling`。

## 等待策略

每个任务携带一个等待策略，用于 task-list 和 task-status 兼容：

| `TaskWaitPolicy` | 行为 |
|------------------|----------|
| `Background` | 任务独立运行；任务活跃期间 Agent 可以继续轮次 |

当前所有任务类型都报告 `Background`。历史任务 detail 载荷可能仍包含
`wait_policy: "blocking"`，但运行时会忽略该值，不用于调度阻塞决策。

**关键契约：**

- 对后台任务，用 `WaitFor(wake=task_result, resource=<task_id>, delivery=...)`
  等待终态 `TaskResult`，不要轮询状态或输出。必须显式选择 `delivery=final`
  或 `delivery=silent`。
- 终态 `TaskResult` 在持有匹配等待或独立重入权限时，作为延续上下文重新进入
  Agent。`InvokeAgent(existing_agent)` 的结果引用原始消息；审阅已投递消息本身。
- 原模型工具 `TaskOutput` 和 CLI `holon task output` 已移除；不再提供其
  `block`/`timeout_ms` 轮内短等。`holon tail --agent` 的有界 follow 是观测，
  不是任务完成等待。

## 监督工具

| 工具 | 用途 |
|------|---------|
| `ListTasks` | 紧凑的活跃任务摘要（仅非终态任务），输出有界 |
| `TaskStatus` | 带元数据的单任务生命周期快照 |
| `TaskInput` | 向交互式任务发送 stdin/后续输入 |
| `TaskStop` | 停止运行中的任务（可能经过 `Cancelling`） |

**关键契约：**

- `ListTasks` 排除终态任务；历史详情用 `TaskStatus`。
- 命令输出只从运行时确认的文件路径按有界字节范围读取；不要从 Agent home
  或 task id 拼接路径。文件不可用时应保留明确的不可用状态，不要猜其他路径。
- 用有界的 `holon tail --agent <agent_id> --include-progress` 观测 assistant
  进度和 brief。它按 Agent 而非单个任务归属输出，不消费结果、不注册任务等待，
  也不唤醒生产者。
- `TaskInput` 只对启用交互式延续（`accepts_input=true`）创建的任务接受输入。
- `TaskStop` 发出停止请求；任务可能先进入 `Cancelling`，再到 `Cancelled`。

### 输出观测不等于完成

对运行时确认的命令文件路径，可以这样进行有界读取：

```bash
dd if="$CONFIRMED_OUTPUT_PATH" bs=1 skip=0 count=8192 status=none
```

`CONFIRMED_OUTPUT_PATH` 必须设为运行时返回的精确宿主机路径，不能推测。
采集可能仅保留头尾并在终态重写文件；运行中的文件不是无损追加流。
需要稳定的最终日志时，先等待任务终止。文件或 tail 的 EOF、`end` 回执、
Agent 空闲都不等于任务完成。

底层 HTTP task-output 路由和运行时 capture/result 服务继续保留；本次只移除
模型工具和 CLI 命令。参见 [CLI 观测参考](/zh-CN/reference/cli.md#agent-输出观测)。

## 与 WorkItem 和等待的区别

任务**是执行句柄**，不是规划对象：

- `Task` 表示正在运行或排队的执行单元。
- `WorkItem` 表示 Agent 正在推进的持久目标。
- `Waiting` 表示 WorkItem 或 Agent 为何无法继续。

任务常常服务于 WorkItem 目标（运行命令、委派给子 Agent），但任务生命周期独立于
WorkItem 生命周期。

## 已解决的缺口

- [Issue #1382](https://github.com/holon-run/holon/issues/1382) 从公开/运行时契约中
  移除了未使用的 `Blocking` 任务等待策略。等待改由
  `WaitFor(wake=task_result, resource=<task_id>, delivery=...)` 加终态任务重入表达。
  原模型/CLI 同步输出短等在 #3534 M2 中移除。
