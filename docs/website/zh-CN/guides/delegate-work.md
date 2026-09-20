---
title: 委派工作给另一个 Agent
summary: 把边界清楚的任务交给子 Agent，等待结果并处理返回内容。
order: 13
---

# 委派工作给另一个 Agent

当一件事可以拆出干净的一部分时，把它交给另一个 Agent，自己继续往下走。本页讲
怎么选择调用方式、怎么等结果，以及拿到返回内容后怎么处理。

委派背后有一套模型：谁能行动、权限覆盖到哪里、结果如何被信任。参见[多 Agent 协作](/zh-CN/concepts/multi-agent-collaboration.md)。

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
