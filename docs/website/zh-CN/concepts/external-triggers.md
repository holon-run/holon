---
title: 外部触发器
summary: Holon Agent 如何通过 webhook 唤醒端点和回调 URL 等待并接收外部事件。
order: 25
---

# 外部触发器

外部触发让 Holon Agent 等待运行时之外的事件——比如 CI 流水线完成、GitHub
webhook，或定时器触发。每个 Agent 在启动时都会拿到一个默认入站回调 URL。
Agent 在它上面等待，外部系统投递事件时恢复。

## 触发模型

Holon 把外部事件当作一等运行时概念。模型分三部分：

1. **默认入站** — 每个 Agent 在启动时拿到一个带能力密钥的默认入站回调 URL。
2. **在触发上等待** — Agent 用 `wake=external` 和外部对象引用调用 `WaitFor`。
   运行时记录等待并让出轮次。
3. **外部投递** — 外部系统向回调 URL POST 时，运行时唤醒 Agent 并投递载荷。

## 投递模式

触发器支持两种投递模式：

| 模式 | 行为 |
|------|----------|
| `enqueue_message` | 外部载荷作为普通消息入队到 Agent 队列 |
| `wake_hint` | 回调只作为唤醒信号；Agent 恢复后自己去检查外部状态 |

当外部系统携带完整事件内容（例如 webhook 载荷）时用 `enqueue_message`。当
Agent 被唤醒后需要主动轮询或检查外部状态（例如 CI 状态检查）时用 `wake_hint`。

## 回调端点

每个 Agent 在启动时都有一个**默认入站能力**。默认入站按 Agent 的配置提供为
`wake_hint` 或 `enqueue_message` 模式的回调 URL。一个触发器始终只有一种投递
模式和一个 `trigger_url`。

| 模式 | URL 模式 | 效果 |
|------|-----------|--------|
| `wake_hint` | `/callbacks/wake/:token` | 把 Agent 从 `WaitFor(external)` 中唤醒 |
| `enqueue_message` | `/callbacks/enqueue/:token` | 入队一条消息（POST 正文） |

回调 token 是**能力密钥**，请像对待密码一样对待它。知道 token 的任何人都能
唤醒你的 Agent 或向它入队消息。Token 由运行时生成，密码学随机，并返回到
Agent 执行环境上下文中（默认不写入对话记录或日志）。

### 示例：唤醒回调

```bash
curl -X POST http://localhost:8787/callbacks/wake/CALLBACK_TOKEN
```

最小的 POST（哪怕空正文）就能唤醒 Agent。运行时校验 token 并恢复等待中的
Agent。

### 示例：入队回调

```bash
curl -X POST http://localhost:8787/callbacks/enqueue/CALLBACK_TOKEN \
  -H "Content-Type: application/json" \
  -d '{"text": "CI build #42 completed: success"}'
```

载荷作为消息入队到 Agent。Agent 在下一个轮次处理它。

## Agent 的 WaitFor 循环

当 Agent 预期一个外部事件时：

1. Agent 使用自己的默认入站能力（暴露在执行环境上下文中）
2. Agent 调用 `WaitFor(wake=external, resource=<object>)`
3. 运行时记录等待、让出轮次，Agent 进入休眠
4. 外部系统通过回调 URL 投递事件
5. 运行时唤醒 Agent，从 WaitFor 处恢复

WaitFor 的 `resource` 参数标明 Agent 在等什么（例如 PR 用
`github:owner/repo#123`，HTTP 端点用 URL）。这仅供 Agent 自己追踪，运行时
不会解释它。

## 安全模型

回调 token 是能力密钥。运行时只保存 token 的哈希，不保存原值——token 一旦
签发，原 token 只返回到 Agent 执行上下文，永远不会写入日志或存储。请在调用方
妥善保存。

触发器可以用 `CancelExternalTrigger` 撤销。撤销会立即让它的 URL 失效，之后
对该 URL 的请求都会被拒绝。

### 重置回调 token

控制端点 `POST /api/control/agents/:agent_id/reset-callback` 会撤销该 Agent
当前的外部触发器，并配发一个带新 token 的新触发器。旧 token 立即失效。token
泄露时可以这样做，也可以作为定期的安全轮换。

## 默认入站能力

每个 Agent 在启动时都会创建一个默认外部入站能力：

- 无需显式创建就有回调 URL（模式由 Agent 配置决定）
- 默认触发器 ID 和回调 token 暴露在 Agent 的执行环境上下文中

这个默认触发器足以应付大多数场景。只有当你需要不同于默认的投递模式，或做
兼容性诊断时，才用 `CreateExternalTrigger`。不要把“创建多个触发器”当成
常规做法。

## 取消触发器

不再需要时撤销触发器：

```
CancelExternalTrigger { external_trigger_id: "trigger_abc123" }
```

这会立即让回调 URL 失效。之后对这些 URL 的任何请求都会被拒绝。

### Agent 停止时自动撤销

Agent 被停止（`holon agent stop`）时，它的所有外部触发器都会自动撤销。这样
可以避免回调 URL 在 Agent 不再运行后仍然活着。之后重新启动 Agent 时，会配发
带新 token 的新触发器。

## 集成模式

### CI/CD 流水线

让 CI 系统在流水线完成时 POST 到唤醒回调。Agent 使用默认入站，启动构建，在
触发器上等待，构建报告状态后恢复。

### GitHub webhook

把 GitHub webhook 指向入队回调。Agent 会把仓库事件（issue、PR、push）当作
入队消息接收，并在下一个轮次处理。

### 定时器

Holon 的定时器生命周期可以用 `holon timer create`、`list`、`cancel` 管理；
旧式创建写法 `holon timer --after-ms 60000` 仍然支持。Agent 可以使用仅限当前
Agent 的 `CreateTimer`、`ListTimers`、`GetTimer`、`CancelTimer` 工具。创建
定时器不会暂停当前轮次；当当前工作必须等该定时器时，配合
`WaitFor(wake=timer, resource=<timer-id>)` 使用。

## 另见

- [集成指南](/zh-CN/guides/integration.md) — HTTP 控制平面端点参考
- [运行时模型](/zh-CN/concepts/runtime-model.md) — 触发器如何融入 Agent 执行循环
- [WaitFor 工具](/zh-CN/reference/model-tool-schema-inventory.md) — 与触发器搭配的 WaitFor 工具
