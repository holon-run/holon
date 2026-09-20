---
title: 多 Agent 协作
summary: 创建和调用 Agent、监督契约，以及用于并行工作的 workspace 模式。
order: 35
---

# 多 Agent 协作

> **心智模型与规格契约：** 理解多 Agent 协同在系统架构中的心智模型，请查阅 [运行时模型](/zh-CN/concepts/runtime-model.md)；查阅父子任务监督与生命周期契约，请参阅维护者专属的 [任务规格](/zh-CN/spec/tasks.md)。

Holon 支持创建可寻址的 Agent，并调用私有的受监督子 Agent，用来完成并行工作、
委托和专门的子任务。

## 协作原语概览

### Agent 操作

| 工具 / 操作 | 返回值 | 适用场景 |
|------------|--------|----------|
| `CreateAgent` | `agent_id` | 创建独立、持久、自属的 Agent 身份（可指定 template、显示名称与引导消息） |
| `InvokeAgent`（新建子 Agent） | `agent_id` + `task_handle` | 运行由父级监督的子任务，具备结果导向生命周期与可选 worktree 隔离 |
| `InvokeAgent`（已有 Agent） | `agent_id` + `task_handle` | 同级调用（Peer Invocation）：向已有授权 Agent 发送消息并等待其下一条持久回复 |
| `SendAgentMessage` | 投递回执 | 向已有授权 Agent 发送异步持久消息，不创建任务等待句柄 |
| `GetAgent` | Agent 摘要 | 读取 Agent 平面状态（身份、显示名称、生命周期、活跃焦点、等待状态与子级血统） |

### Agent 标识与显示名称

- **永久 Agent ID**：规范标识符（如 `reviewer`、`builder`）是持久且不可更改的。
- **显示名称（Display Name）**：自属的公开 Agent 可拥有人类可读的显示名称，可通过 CLI（`holon agent rename <id> --name <name>`）或 HTTP API（`PATCH /api/control/agents/:id/name`）修改。默认 Agent 不可重命名。
- **Incarnation（化身代际）**：跟踪 Agent 生命周期重置与运行时重载代际的持久序列号。

### Workspace 模式

| 模式 | 说明 |
|------|------|
| `inherit`（默认） | 子 Agent 共用父 Agent 的 workspace |
| `worktree` | 子 Agent 获得独立 worktree，用于安全实验 |

### 任务句柄监督

调用 `InvokeAgent` 时，调用方会拿到一个带 `task_id` 的 `task_handle`。

对于**新建子 Agent**（`kind: "new_subagent"`），句柄代表由父级监督的子任务，会产生最终交付结果。可以用它做这些事：

- **TaskStatus** — 查看生命周期、等待状态和元数据
- **TaskOutput** — 读取有界输出，或等待完成
- **TaskInput** — 向子 Agent 发送后续输入
- **TaskStop** — 显式停止子 Agent

对于**已有 Agent**（`kind: "existing_agent"`），句柄等待目标 Agent 发出的第一条后续持久消息。满足等待条件但不是父子生命周期包含边界。

## 调用风格

### 受监督子任务（Subagent）

创建由当前 Agent 严格监督的私有附属 Agent：

```json
{
  "target": {
    "kind": "new_subagent",
    "template": "code-reviewer",
    "workspace_mode": "worktree"
  },
  "initial_message": "审查 src/runtime/ 下的 Pull Request 改动"
}
```

### 同级调用（Peer Invocation）

作为对等实体调用长期存在的已有 Agent：

```json
{
  "target": {
    "kind": "existing_agent",
    "agent_id": "auditor"
  },
  "initial_message": "请审计运行时 SQLite 数据库的保留策略规则。"
}
```

### 子 Agent 的 token 用量

任务状态和输出快照都带一个 `token_usage` 字段，记录子 Agent 的累计 token
消耗：

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

| 字段 | 说明 |
|------|------|
| `total` | 子 Agent 所有轮次的累计 token |
| `total_model_rounds` | 子 Agent 已经历的模型往返次数 |
| `last_turn` | 最近一轮的 token 用量（如果可用） |

用 token 用量估算子 Agent 的成本、发现异常昂贵的委托，或者判断某个子 Agent
消耗的 token 是否已经远超它的产出、该不该停掉。

## 使用模式

### 并行调查

同时调用多个 Agent，各自探查不同的方面：

```
父 Agent：
  InvokeAgent("Review src/runtime/ for performance issues")
  InvokeAgent("Review src/runtime/ for error handling gaps")
  InvokeAgent("Review src/runtime/ for missing tests")
  → 等待所有任务句柄完成
  → 把发现汇总成最终报告
```

### 专门化的委托

针对不同的关注点指派专门化的 Agent：

```
父 Agent：
  InvokeAgent("Code review", template="code-reviewer")
  InvokeAgent("Test writing", template="test-writer")
```

### 安全实验

用 `worktree` 模式让子 Agent 在不影响主 workspace 的情况下做实验：

```
父 Agent：
  InvokeAgent("Try alternative implementation approach",
              workspace_mode=worktree)
  → 子 Agent 在独立 worktree 里工作
  → 父 Agent 审阅子 Agent 的输出
  → 父 Agent 把最好的做法应用到主 workspace
```

## 监督流程

一次典型的父子交互：

1. **调用** — 父 Agent 用 `InvokeAgent` 发起调用，用 `initial_message` 描述任务
2. **监控** — 父 Agent 用 `TaskStatus` 检查子 Agent 是在工作、休眠，还是在等待
3. **审阅** — 父 Agent 读 `TaskOutput` 获取有界预览，或等待完成
4. **交付** — 父 Agent 把子 Agent 的结果汇总成最终面向用户的答案

父 Agent 始终负责：

- **验证** — 子 Agent 的输出是证据，不是权威
- **汇总** — 合并多个子 Agent 的结果
- **最终交付** — 由父 Agent 产出面向用户的答案

## 最佳实践

- **让委托保持聚焦。** 每个子 Agent 只应有一个明确目标。
- **显式监督。** 在假定任务完成之前先检查 `TaskStatus`。
- **把子 Agent 的输出当作证据。** 交给用户之前先复核、验证。
- **限制并行度。** 只调用任务真正用得上的 Agent 数量。
- **停掉闲置的子 Agent。** 对不再需要的子 Agent 用 `TaskStop`。

## 另见

- [运行时模型](/zh-CN/concepts/runtime-model.md) — Agent 生命周期和任务监督
- [信任边界](/zh-CN/concepts/trust-boundaries.md) — 为什么子 Agent 的输出是证据，而不是权威
- [工作项指南](/zh-CN/guides/work-items.md) — 跨 Agent 跟踪目标
