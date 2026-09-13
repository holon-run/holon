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
> task_output,task_input,task_stop,invoke_agent}.rs`.

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

- 对后台任务，用 `WaitFor(wake=task_result, resource=<task_id>)` 等待终态
  `TaskResult`，而不是轮询 `TaskOutput`。
- 终态 `TaskResult` 事件作为延续上下文重新进入 Agent；运行时会自动唤醒 Agent。
- `TaskOutput(block=true)` 用于当前轮次内的显式同步等待，不是默认等待策略。

## 监督工具

| 工具 | 用途 |
|------|---------|
| `ListTasks` | 紧凑的活跃任务摘要（仅非终态任务），输出有界 |
| `TaskStatus` | 带元数据的单任务生命周期快照 |
| `TaskOutput` | 有界输出预览，可选 `block=true` |
| `TaskInput` | 向交互式任务发送 stdin/后续输入 |
| `TaskStop` | 停止运行中的任务（可能经过 `Cancelling`） |

**关键契约：**

- `ListTasks` 排除终态任务；历史详情用 `TaskStatus`。
- `TaskOutput` 返回有界的 `output_preview` 和指向完整输出的工件引用；它用于查看，
  不是轮询。
- `TaskInput` 只对启用交互式延续（`accepts_input=true`）创建的任务接受输入。
- `TaskStop` 发出停止请求；任务可能先进入 `Cancelling`，再到 `Cancelled`。

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
  `WaitFor(wake=task_result)` 加终态任务重入表达，或在当前轮次内用有界的
  `TaskOutput(block=true)` 调用表达。
