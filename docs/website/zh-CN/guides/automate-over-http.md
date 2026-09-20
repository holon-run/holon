---
title: 通过 HTTP 自动化 Holon
summary: 从代码里完成认证、提交工作、跟踪状态并读取结果。
order: 14
---

# 通过 HTTP 自动化 Holon

Holon 的 HTTP 控制平面让脚本或服务在不打开 TUI 的情况下驱动 Agent、任务和工作项。
本页启动服务端、完成认证，并走通一个请求到结果。所有路由都在 `/api` 前缀下。

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

完整的端点列表、请求体和认证要求见 [HTTP 控制平面参考](/zh-CN/reference/http-control-plane.md)。先在那里找到路由，再回到本页看流程。

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

每个请求都带有来源和信任级别，运行时不会把它们混在一起。分类方式见[信任边界](/zh-CN/concepts/trust-boundaries.md)。

Operator transport binding 等高级控制项见 [HTTP 控制平面参考](/zh-CN/reference/http-control-plane.md)。

## 另请参阅

- [HTTP 控制平面参考](/zh-CN/reference/http-control-plane.md) — 设计目标与端点细节
- [CLI 参考](/zh-CN/reference/cli.md) — 等价的命令行操作
- [配置参考](/zh-CN/reference/configuration.md) — 服务端与运行时配置
