---
title: 运行长生命周期任务
summary: 启动需要等待的工作，查看进度，断线后恢复，并取得最终交付。
order: 12
---

# 运行长生命周期任务

有些工作应该活得比终端更久：跑一个小时的构建、等 review 的 pull request、等外部
事件的任务。本页带你从启动走到最终交付。

Holon 让工作在 daemon 上持续，而不是依赖你的连接。原因见[上下文连续性](/zh-CN/concepts/context-continuity.md)；本页只讲操作步骤。

## 端到端工作流

### 1. 启动 daemon

所有持久工作都需要 Holon daemon：

```bash
holon daemon start
holon daemon status
```

daemon 会在 `~/.holon/agents/main/` 创建一个默认 Agent，并监听本地 Unix
socket。

### 2. 创建专用 Agent

要做专注的持久工作，就用已安装或已同步的模板创建一个具名 Agent：

```bash
holon agent create builder --template software-developer
```

这会依据本地 developer 模板创建 `~/.holon/agents/builder/`。

### 3. 启动一个由工作项支撑的任务

通过 TUI 连接到 Agent 并开始跟踪工作：

```bash
holon tui
```

在 TUI 里告诉 Agent 要做什么。Agent 可以创建一个**工作项**：用计划、todo
清单和完成标准跟踪的持久目标。工作项能熬过 TUI 断连和 daemon 重启。

示例提示词：

```
I need to refactor the error handling in src/runtime/turn.rs.
Create a work item, plan the approach, and start implementing.
```

Agent 创建工作项、写下计划、开始动手。你随时可以断开 TUI（`Ctrl+C`），
Agent 会继续。

### 4. 从另一个会话查看进度

之后再连回来，或者从另一个终端连：

```bash
# 快速查看状态（不需要 TUI）
holon agent status builder

# 重新连接 TUI 交互
holon tui
```

### 5. 等待外部事件

长任务经常需要等待。Holon 处理三类等待：

#### 等待命令结束

当 Agent 运行 shell 命令（构建、测试、lint）时，命令会变成后台任务。Agent
可以继续做别的事，或者休眠到任务完成：

```
> Run cargo test and wait for the results

[Agent runs tests as a background task, sleeps, and wakes when tests finish]
```

#### 等待操作者输入

当 Agent 需要做决定时，它把工作项设为 `plan_status=needs_input` 并休眠：

```
> I've found two possible approaches for the error refactor.
  Option A: use thiserror
  Option B: manual Display impls
  Which should I use?
```

Agent 休眠，工作项停在 “needs input” 状态。你可以之后通过 TUI、CLI 提示词或
HTTP API 回应。

#### 等待外部触发

对于 CI、webhook 或定时事件，Holon 用 `WaitFor` 加上所需的外部触发器来唤醒
Agent。Agent 把外部对象记录在 `resource` 里，并写明人类可读的 `reason`：

```
> I've pushed the branch. Let's wait for CI to complete before merging.
  [WaitFor: wake=external resource=github:owner/repo#ci-run reason="CI check on feature/error-refactor"]
```

当 CI 完成、配置好的触发器触发时，Agent 被唤醒、检查 CI 状态、继续推进；
如果 CI 失败就更新工作项。

### 6. 处理中断

持久工作能熬过常见中断：

| 场景 | 结果 |
|----------|-------------|
| TUI 断连 | Agent 继续运行；daemon 保持在线 |
| daemon 重启 | Agent 状态、工作项和 Agent home 都持久化在磁盘上 |
| 机器重启 | 执行 `holon daemon start` 后工作恢复 |
| 命令任务仍在运行 | 任务在后台继续；Agent 可以查看或等待 |

### 7. 接收最终简报

工作项完成时，Agent 会写一份**完成简报**，结构化总结做了什么、为什么做，
以及验证结果：

```
WorkItem complete: refactor error handling in src/runtime/turn.rs

Changes:
- Replaced manual error strings with thiserror derive macros
- Added structured error variants for turn, queue, and task errors
- Updated 12 call sites to use new error types

Verification: cargo test --all-targets passes (124 tests, 0 failures)
```

随时查看已完成的工作：

```bash
holon transcript          # 完整对话历史
holon agent status builder  # Agent 状态和近期活动
```

## 什么时候用持久工作流

| 场景 | 做法 |
|----------|---------|
| 多步代码改动 | 带计划和 todo 清单的工作项 |
| CI 驱动的工作流 | WaitFor(external) + 外部触发器 |
| 评审循环 | 用 needs_input 等操作者评审 |
| 跨会话项目 | 带 workspace 的具名 Agent |
| 后台自动化 | daemon + Agent + 触发器 |

不需要持久性的快速一次性任务，用 `holon run`。

## 另见

- [工作项指南](/zh-CN/guides/work-items) — 工作项生命周期和最佳实践
- [多 Agent 协作](/zh-CN/guides/multi-agent) — 把工作委派给子 Agent
- [CLI 参考](/zh-CN/reference/cli) — 完整命令面
- [运行时模型](/zh-CN/concepts/runtime-model) — 持久性背后的概念
- [集成指南](/zh-CN/guides/integration) — 用于自动化的 HTTP API
