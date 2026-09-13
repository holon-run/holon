---
title: 集成指南
summary: 通过 curl 示例和端点参考，以编程方式访问 Holon 的 HTTP 控制平面。
order: 25
---

# 集成指南

Holon 提供 HTTP 控制平面用于编程访问。用 `holon serve` 启动服务端，就能通过类 REST 的 API 操作 Agent、任务和工作项。所有路由都挂在 `/api` 前缀下。

## 启动服务端

```bash
# 仅本机访问
holon serve --port 8787

# 启用 token 认证
holon serve --port 8787 --token "your-secret-token"
```

访问模式：`local`、`tunnel`、`lan`、`tailnet`。默认监听地址是 `127.0.0.1:7878`。监听非回环地址，或使用 `lan`/`tailnet` 访问，都必须提供 token。

## API 约定

- **Base URL：** `http://localhost:8787`
- **路由前缀：** `/api`
- **Content-Type：** `application/json`
- **认证：** 设置 `--token` 后，在 `Authorization` 头中携带 Bearer token

## 核心端点

### Agent 管理

| 方法 | 路径 | 说明 |
|--------|------|-------------|
| `GET` | `/api/agents/list` | 列出带元数据的活跃 Agent 条目 |
| `POST` | `/api/control/agents/:agent_id/create` | 创建新 Agent |
| `GET` | `/api/agents/:agent_id/status` | 获取 Agent 状态与生命周期 |
| `GET` | `/api/agents/:agent_id/state` | 获取轻量级 Agent 状态引导数据 |

### 消息

| 方法 | 路径 | 说明 |
|--------|------|-------------|
| `POST` | `/api/agents/:agent_id/enqueue` | 向 Agent 入队一条消息 |
| `POST` | `/api/control/agents/:agent_id/prompt` | 发送 operator 提示词 |
| `POST` | `/api/control/agents/:agent_id/wake` | 唤醒休眠中的 Agent |
| `POST` | `/api/control/agents/:agent_id/control` | 发送控制指令 |

### 任务与工作项

| 方法 | 路径 | 说明 |
|--------|------|-------------|
| `POST` | `/api/control/agents/:agent_id/tasks` | 创建命令任务 |
| `POST` | `/api/control/agents/:agent_id/work-items` | 创建工作项 |
| `POST` | `/api/control/agents/:agent_id/work-items/:work_item_id/pick` | 选定当前工作项 |
| `PATCH` | `/api/control/agents/:agent_id/work-items/:work_item_id` | 更新工作项 |
| `POST` | `/api/control/agents/:agent_id/work-items/:work_item_id/complete` | 完成工作项 |
| `GET` | `/api/agents/:agent_id/tasks` | 列出 Agent 任务 |
| `GET` | `/api/agents/:agent_id/briefs` | 获取近期简报/上下文 |
| `GET` | `/api/agents/:agent_id/transcript` | 获取 Agent 对话记录 |
| `GET` | `/api/agents/:agent_id/events` | 获取 Agent 事件流 |

### Workspace 与 Skills

| 方法 | 路径 | 说明 |
|--------|------|-------------|
| `POST` | `/api/control/agents/:agent_id/workspace/attach` | 挂载 workspace |
| `POST` | `/api/control/agents/:agent_id/workspace/detach` | 卸载 workspace |
| `GET` | `/api/agents/:agent_id/skills` | 列出 Agent 已启用的 skills |
| `POST` | `/api/control/agents/:agent_id/skills/enable` | 为 Agent 启用 skill |
| `POST` | `/api/control/agents/:agent_id/skills/disable` | 为 Agent 禁用 skill |
| `GET` | `/api/skills/catalog` | 列出 Skill Library 目录 |
| `POST` | `/api/skills/catalog/add` | 向库中添加 skill |
| `POST` | `/api/skills/catalog/remove` | 从库中移除 skill |
| `POST` | `/api/skills/catalog/reconcile` | 按 lock 文件对账库内容 |

### 回调与 Webhook

| 方法 | 路径 | 说明 |
|--------|------|-------------|
| `POST` | `/api/callbacks/enqueue/:callback_token` | 携带载荷的外部回调 |
| `POST` | `/api/callbacks/wake/:callback_token` | 外部唤醒触发器 |
| `POST` | `/api/webhooks/generic/:agent_id` | 通用 webhook 入口 |

### 运行时控制

| 方法 | 路径 | 说明 |
|--------|------|-------------|
| `GET` | `/api/control/runtime/status` | 运行时健康状态 |
| `POST` | `/api/control/runtime/shutdown` | 优雅关闭 |

## 示例

### 向 Agent 发送消息

```bash
curl -X POST http://localhost:8787/api/agents/my-agent/enqueue \
  -H "Content-Type: application/json" \
  -d '{
    "text": "Review the latest changes in src/",
    "priority": "normal",
    "origin": {
      "kind": "webhook",
      "source": "my-service"
    }
  }'
```

公开 enqueue 路由只接受 `channel` 或 `webhook` 来源，并拒绝 `interject` 优先级。要让消息带有 operator 信任，请改用下面的 operator transport binding。

响应：

```json
{
  "ok": true,
  "agent_id": "my-agent",
  "message_id": "msg_abc123"
}
```

### 创建 Agent

```bash
curl -X POST http://localhost:8787/api/control/agents/reviewer/create \
  -H "Content-Type: application/json" \
  -d '{"template": null}'
```

### 查看 Agent 状态

```bash
curl http://localhost:8787/api/agents/my-agent/status
```

### 创建工作项

```bash
curl -X POST http://localhost:8787/api/control/agents/my-agent/work-items \
  -H "Content-Type: application/json" \
  -d '{"objective": "Review and fix all clippy warnings"}'
```

### 更新并完成工作项

```bash
curl -X PATCH http://localhost:8787/api/control/agents/my-agent/work-items/work_123 \
  -H "Content-Type: application/json" \
  -d '{
    "plan_status": "ready",
    "todo_list": [
      { "text": "Run cargo check", "state": "completed" }
    ],
    "blocked_by": "waiting for CI",
    "recheck_after": 600000
  }'

curl -X POST http://localhost:8787/api/control/agents/my-agent/work-items/work_123/complete \
  -H "Content-Type: application/json" \
  -d '{"report_text": "Build fixed and all checks passed."}'
```

### 创建并取消定时器

```bash
curl -X POST http://localhost:8787/api/control/agents/my-agent/timers \
  -H "Content-Type: application/json" \
  -d '{"duration_ms": 60000, "summary": "reminder"}'

curl -X POST http://localhost:8787/api/control/agents/my-agent/timers/timer_123/cancel \
  -H "Content-Type: application/json" \
  -d '{}'
```

### 唤醒休眠中的 Agent

```bash
curl -X POST http://localhost:8787/api/control/agents/my-agent/wake \
  -H "Content-Type: application/json" \
  -d '{
    "reason": "CI build completed",
    "source": "github-actions"
  }'
```

### 列出 Agent 任务

```bash
curl http://localhost:8787/api/agents/my-agent/tasks
```

### 获取 Agent 对话记录

```bash
curl "http://localhost:8787/api/agents/my-agent/transcript?limit=50"
```

## 信任与来源

每条入站消息都带一个 `origin`，用于标注来源。运行时据此执行信任边界：

- `operator` — 通过可信渠道接入的人类 operator
- `channel` — 外部集成渠道
- `webhook` — 第三方 webhook
- `callback` — 运行时投递的外部触发器回调
- `timer` — 定时器触发
- `system` — 运行时内部子系统
- `task` — 子任务完成

消息还携带 `priority`（`interject`、`next`、`normal`、`background`）和信任级别元数据。

## Operator transport binding

持久集成渠道可以注册 operator transport binding：

```bash
curl -X POST http://localhost:8787/api/control/agents/my-agent/operator-bindings \
  -H "Content-Type: application/json" \
  -d '{
    "transport": "http_callback",
    "operator_actor_id": "slack-bot-01",
    "default_route_id": "slack-channel-general",
    "delivery_callback_url": "https://my-service.example.com/holon-delivery",
    "delivery_auth": {
      "kind": "bearer",
      "bearer_token": "my-delivery-token"
    },
    "capabilities": {
      "text": true,
      "markdown": true
    }
  }'
```

绑定后，用 operator ingress 端点转发消息：

```bash
curl -X POST http://localhost:8787/api/control/agents/my-agent/operator-ingress \
  -H "Content-Type: application/json" \
  -d '{
    "text": "User asked: can you explain the build error?",
    "actor_id": "slack-bot-01",
    "binding_id": "binding-abc"
  }'
```

## 另请参阅

- [HTTP 控制平面参考](/zh-CN/reference/http-control-plane.md) — 设计理念与核心概念
- [CLI 参考](/zh-CN/reference/cli.md) — 等价的命令行操作
- [配置参考](/zh-CN/reference/configuration.md) — 服务端与运行时配置
