---
title: HTTP 控制平面
summary: 如何理解 Holon 的无头集成接口。
order: 20
---

# HTTP 控制平面

Holon 在设计上是无头的。HTTP 和事件驱动的集成接口应当保留与 CLI 相同的运行时概念：origin、trust、priority、工作项、任务、队列、唤醒以及面向用户的交付。

当前生成的 OpenAPI 基线已检入到
[`openapi.json`](/reference/openapi.json)。它是针对当前路由接口的一份保守 schema；在第二阶段的路由/类型元数据和 DTO 契约稳定之前，部分请求和响应 schema 会有意保持宽泛。

## 认证

配置了 control token 后（例如 `--token`、`--token-file`，或
`control_token` 配置键），HTTP 服务端进入 **bearer 模式**。
所有 `/api/control/*` 路由都要求 `Authorization: Bearer <token>` 头，
只读路由（agent 状态、事件、任务）在远程访问时也要求该头。没有 control token
时，服务端运行在 **local 模式**，信任本地进程边界。

从 v0.36.0 开始，Holon 支持**优先 Session（Session-first）**的认证架构。除 Bearer Token 外，浏览器和 Web UI 客户端还支持通过 HTTP-only Session Cookie 进行认证：

- **`GET /api/auth/method`** — 返回当前认证模式（`"local"` 或 `"oidc"`）。
- **`POST /api/auth/session/exchange`** — 使用有效 Token 换取 HTTP-only Session Cookie。
- **`GET /api/auth/session/me`** — 查询当前已认证 Session 的身份、角色与过期时间。
- **`POST /api/auth/session/logout`** — 注销当前 Session 并清除 Session Cookie。
- **`GET /api/auth/oidc/start`** 与 **`GET /api/auth/oidc/callback`** — 当 `auth.mode="oidc"` 时发起并完成 OpenID Connect PKCE 授权码流程。
- **`POST /api/auth/:provider/device/start`** — 发起 OAuth 设备授权码流程（如 OpenAI Codex）。

```
GET /api/handshake → { "auth": { "mode": "bearer" | "local", "required": bool } }
GET /api/auth/method → { "mode": "local" | "oidc" }
```

## 入口信任与认证边界

Holon 把认证、消息来源、信任级别、优先级和权威当作彼此独立的运行时事实。HTTP
入口处理器可以认证调用方，但仍会显式构造消息的来源信息，而不是信任调用方提供的来源字段。

| 入口类别 | 路由 | 认证边界 | 来源/信任/权威 | 优先级 | 支持的状态 |
|---------------|--------|---------------|------------------------|----------|-------------------|
| 公共 enqueue | `POST /api/enqueue`、`POST /api/agents/:id/enqueue` | bearer 模式下用 Bearer token；local 模式下用本地进程边界。 | 调用方只能提供 channel 或 webhook 来源。调用方提供的 `trust` 会被拒绝。channel 来源成为不可信的外部证据；webhook 来源成为集成信号。`system_tick`、`callback_event` 等运行时自有类型会被拒绝。 | `next`、`normal` 或 `background`；`interject` 会被拒绝。 | 面向非 operator 证据的候选稳定外部入口。 |
| 回调能力 | `POST /api/callbacks/wake/:callback_token`、`POST /api/callbacks/enqueue/:callback_token` | URL 路径中的 capability token 解析为一个活跃的外部触发器并匹配投递模式。不要记录、重复或发布完整的回调 URL。 | 投递作为外部触发器能力和集成信号被接纳。wake 回调会入队运行时自有的检查 tick；回调 payload 文本是供 agent 检查的不可信证据。 | 由投递模式在运行时选择；调用方不选择队列优先级。 | 面向需要唤醒或通知 agent 的持久外部系统的能力接口。 |
| Operator 传输绑定 | `POST /api/control/agents/:id/operator-bindings` | 控制平面认证。投递凭据存储在绑定上，并从审计事件中脱敏。 | 创建或更新绑定，该绑定随后授权远程 operator 入口。 | 不适用。 | 实验性的 operator 适配器设置接口。 |
| Operator 传输入口 | `POST /api/control/agents/:id/operator-ingress` | 控制平面认证，外加活跃绑定、匹配的目标 agent、匹配的 operator 参与者，以及（在提供时）匹配的 provider。 | 入队一条 `trusted_operator` 的 `operator_prompt`，带有 `operator_instruction` 权威和远程 operator 传输元数据。 | 始终为 `interject`。 | 实验性的已认证 operator 适配器入口。 |
| 通用 webhook 兼容 | `POST /api/webhooks/generic/:agent_id` | bearer 模式下用 Bearer token；local 模式下用本地进程边界。 | 把 JSON payload 转换为一个来源为 `generic_webhook` 的可信集成 webhook 事件。调用方无法通过该路由设置来源、信任或优先级。 | 始终为 `normal`。 | 内部/调试兼容路由；新外部集成应优先使用公共 enqueue 或专用的能力回调。 |

## 端点参考

### 发现

**`GET /api/`** — 根

返回默认 agent ID。

```json
{ "ok": true, "default_agent": "main" }
```

**`GET /api/handshake`** — 协议握手

返回认证模式、能力和运行时信息。

```json
{
  "ok": true,
  "protocol": { "name": "holon-control", "version": 1 },
  "auth": { "mode": "bearer", "required": true },
  "capabilities": ["agents.list", "agents.state", "agents.events", "agents.control", "tui.remote"],
  "runtime": {
    "default_agent": "main",
    "workspace_dir": "/path/to/workspace",
    "home_dir": "/path/to/holon/home",
    "listen": "127.0.0.1:7878",
    "advertise_url": null
  }
}
```

**`GET /api/models`** — 可用模型

返回缓存的模型目录和运行时可用性，不联系 provider。

**`POST /api/models/refresh`** — 刷新可用模型

为发现缓存缺失或过期的 provider 发现模型，然后返回与 `GET /api/models` 相同的目录结构。
某个 provider 发现失败不会阻止其他可用模型被返回。

```json
{
  "available_models": [
    { "id": "claude-sonnet-4-20250514", "display_name": "Claude Sonnet 4", … }
  ],
  "model_availability": { "claude-sonnet-4-20250514": true, … }
}
```

### Agent

**`GET /api/agents/list`** — 列出 agent 条目

返回轻量的公共 agent 条目，供选择和导航使用，不加载每个 agent 完整的运行时摘要。

**`GET /api/agents/:id/status`** — 单个 agent 状态

为指定 agent 返回相同的 `AgentSummary` 结构。

**`GET /api/agents/:id/state`** — 轻量 agent 状态引导

返回一个有界引导页：agent 摘要、会话信息（当前 run、待处理数量）、活跃任务摘要、近期 timer、
精简工作项、等待意图、外部触发器和 workspace 占用情况。Operator 通知、执行详情、
任务详情和完整工作项记录可从事件或专用路由获取。

**`GET /api/agents/:id/briefs`** — 近期简报

返回该 agent 的近期简报（确认和结果）。

**`GET /api/agents/:id/tasks`** — 活跃任务

返回带状态、类型和计时元数据的活跃及近期任务。

**`GET /api/agents/:id/tasks/:task_id`** — 任务状态

返回某个受管任务的结构化任务生命周期快照。

**`GET /api/agents/:id/tasks/:task_id/output`** — 任务输出

返回有界的任务输出结果。查询参数：

| 参数 | 说明 |
|-------|-------------|
| `block` | 是否在返回前等待输出/完成 |
| `timeout_ms` | `block=true` 时可选的、有界的等待时长 |

**`GET /api/agents/:id/timers`** — 近期 timer

返回近期 timer 记录。

**`GET /api/agents/:id/conversation`** — 对话读模型概要

返回对话总览、当前轮次状态与可见历史消息。

**`GET /api/agents/:id/conversation/stream`** — 对话流式更新

用于监听活跃对话轮次、流式文本输出与模型思考过程的 Server-Sent Events 流。

**`GET /api/agents/:id/turns/:turn_id/activities`** — 轮次活动详情

返回特定对话轮次中细粒度的工具调用与内部活动。

**`GET /api/agents/:id/timers/:timer_id`** — Timer 详情

按 id 返回单个 timer 记录；当目标 agent 下找不到该 timer 时，返回共享的错误信封。

**`GET /api/agents/:id/events`** — 事件日志

返回近期运行时事件（turn 条目、系统事件）。查询参数：

| 参数 | 说明 |
|-------|-------------|
| `before_seq` | 返回持久 `event_seq` 低于该值的事件 |
| `after_seq` | 返回持久 `event_seq` 高于该值的事件 |
| `limit` | 最多返回的事件数（默认 128） |
| `order` | `asc` 或 `desc`（默认） |
| `max_level` | 可选的纳入过滤：`info`、`verbose` 或 `debug` |

JSON 响应是一个 `EventsPageResponse`：

| 字段 | 契约 |
|-------|----------|
| `events` | 匹配所请求级别过滤的完整 payload `StreamEventEnvelope` 记录数组 |
| `oldest_seq` / `newest_seq` | 返回页中最低/最高的持久 `event_seq`；空页时为 `null` |
| `cursor_seq` | 在服务该页时捕获的原始事件日志高水位；客户端可在此游标之后开始原始流 |
| `has_older` / `has_newer` | 在返回窗口之前/之后是否还有更多匹配记录 |
| `order` | 回显所请求的 order |
| `limit` | 服务端钳制后的有效 limit |

`before_seq` 和 `after_seq` 是排他游标。两者都提供时，页面包含满足
`after_seq < event_seq < before_seq` 的事件。事件页从持久事件日志加载，因此未知游标
可能得到空页，而不是游标错误。

**`GET /api/agents/:id/events/stream`** — 服务端发送事件

原始 agent 事件的 SSE 流。支持 `after_seq` 和 `limit` 查询参数。
SSE 的 `id` 字段是每个 agent 的持久 `event_seq`，SSE 的 `event` 字段被设为
原始审计事件类型（例如 `turn_entry`、`wake_requested`、`task_create_requested`），
而不是一小组固定名称。

每个 SSE `data` 帧是一个 JSON `StreamEventEnvelope`，包含以下稳定字段：

| 字段 | 契约 |
|-------|----------|
| `id` | 审计事件 id。它不是 SSE 帧 id。 |
| `event_seq` | 每个 agent 的持久序列号；等于 SSE 的 `id` 字段 |
| `ts` | RFC 3339 时间戳 |
| `agent_id` | 拥有该事件日志的 agent |
| `type` | 原始审计事件类型；等于 SSE 的 `event` 字段 |
| `provenance` | 在原始 payload 中存在时，从中提取的稳定来源字段 |
| `payload` | 完整事件 payload |

省略 `after_seq` 时，流从当前尾部之后开始，只发出未来事件。`after_seq=0` 时，
从当前重放窗口的开头重放。对于非零 `after_seq`，游标必须仍在重放窗口内；否则在打开
SSE 流之前，路由返回 `404`，带 `code: cursor_not_found` 以及
`after_seq`/`event_seq` 扩展字段。如果已经打开的流落后于重放窗口，服务端会关闭该流。
客户端必须把 EOF 视为可能的缺口信号：在最后一个连续的 SSE `id` 之后拉取持久事件页，
然后从恢复的高水位重新打开流。

**`GET /api/events/stream`** 是一个跨公共 agent 的仅实时流。它没有全局持久游标，
也没有历史重放。如果它的接收方滞后，服务端会关闭该流；客户端必须在重新打开全局流之前，
从每个受影响 agent 最后一个连续的 `event_seq` 回填该 agent。

过滤行为：

- 事件 payload 始终完整包含。
- `/api/agents/:id/events` 可以用 `max_level` 过滤返回哪些事件。
- `/api/agents/:id/events/stream` 是原始的，不支持 `max_level`。

迁移说明：

- 这是相对早期投影契约的一次破坏性事件 API 变更。请移除 `projection=operator`
  和 `projection=local_debug` 查询参数。
- `StreamEventEnvelope.projection` 不再存在。检查过
  `projection.raw_payload_included` 的客户端应把所有事件信封视为
  完整 payload 信封。
- 要复现旧的面向 operator 的页面密度，请请求
  `/api/agents/:id/events?max_level=info`。流客户端应继续订阅
  `/api/agents/:id/events/stream`，并在本地应用任何展示层过滤。

**`GET /api/agents/:id/transcript`** — Turn 记录

返回当前 turn 的记录条目。

**`GET /api/agents/:id/worktree-summary`** — Worktree 摘要

返回该 agent workspace 的受管 worktree 条目。

### Enqueue（公共入口）

**`POST /api/agents/:id/enqueue`** — 入队一条消息

在公共 HTTP 接口上接受外部调用方。当服务端处于 **bearer 模式**时，
该路由会调用 `authorize_remote_access`，并像只读路由一样要求 control token。
在 **local 模式**下不需要认证头。

运行时对来源、信任和优先级进行分类；公共调用方不得覆盖 trust 或使用
`interject` 优先级。
请求结构：

```json
{
  "kind": "channel_event | webhook_event",
  "priority": "next | normal | background",
  "text": "plain text body",
  "json": { "structured": "body" },
  "body": { "type": "text", "text": "…" },
  "origin": {
    "kind": "channel",
    "channel_id": "slack-general",
    "sender_id": "U123"
  },
  "metadata": {},
  "correlation_id": "optional-correlation",
  "causation_id": "optional-causation"
}
```

响应：

```json
{ "ok": true, "agent_id": "main", "message_id": "msg-abc123" }
```

**`POST /api/enqueue`**（路径中无 agent）— 入队到默认 agent。

**`POST /api/webhooks/generic/:agent_id`** — 通用 webhook 兼容

接受一个 JSON payload 并将其转换为来源为 `generic_webhook` 的
`webhook_event`。该路由保留用于本地/调试兼容和简单的可信集成测试。启用 bearer 模式时
它要求 bearer token，但与公共 enqueue 不同，它不允许调用方提供显式的
origin/trust/priority 字段。新集成通常应使用公共 enqueue 承载外部证据，或在集成
需要密钥 URL 时使用回调能力。

### 控制平面（已认证）

bearer 模式下，所有 `/api/control/*` 路由都要求 control token。

**`POST /api/control/agents/:id/prompt`** — 发送 operator 提示词

发送一个以 `trusted_operator` 分类进入 agent 队列的 operator 消息。

```json
{ "text": "What is the current status?" }
```

**`POST /api/control/agents/:id/wake`** — 显式唤醒

用控制平面唤醒提示唤醒一个休眠中的 agent。

```json
{ "reason": "manual-wake", "source": "operator" }
```

响应：

```json
{ "ok": true, "agent_id": "main", "disposition": "woken" }
```

**`POST /api/control/agents/:id/control`** — 控制动作

发送一个控制动作。请求体：

```json
{ "action": "stop", "authority_class": "operator_instruction" }
```

**`POST /api/control/agents/:id/current-run/abort`** — 中止当前 run

中止当前 agent 运行循环。新调用方应使用
`mode: "stop_after_abort"`。旧的 `pause_after_abort` 值作为兼容别名被接受，
并按 `stop_after_abort` 处理。

```json
{ "mode": "stop_after_abort" }
```

**`POST /api/control/agents/:id/create`** — 创建 agent

创建一个由宿主管理的 agent。agent id 来自 URL 路径。

```json
{ "template": null, "authority_class": "operator_instruction" }
```

创建某个删除任务已完全完成的 id 会开启一个新化身
（`identity.incarnation` 递增；旧状态绝不会被复活）。当删除仍在进行中或已失败时，
请求会以 `409` / `deletion_incomplete` 失败。

**`GET /api/control/agents/tree`** — Agent 层次结构树

返回完整的 Agent 树，包括公开自属 Agent、受监督子 Agent 及其血统关系。

**`GET /api/control/agents/:id/detail`** — 规范 Agent 详情

返回 Agent 的完整规范投影：身份、显示名称、化身代际、配置、模型覆盖、状态与血统。

**`PATCH /api/control/agents/:id/name`** — 重命名 Agent 显示名称

更新公开自属 Agent 的显示名称。规范 `agent_id` 保持不变。

```json
{ "name": "Lead Reviewer" }
```

**`POST /api/control/agents/:id/repair`** — 修复 Agent 引导步骤

重试创建后未完成的引导步骤（模板文件、工作区绑定、初始消息），无需重新创建 Agent 身份。

**`DELETE /api/control/agents/:id`** — 永久删除 Agent

调度异步删除 Agent 及其关联数据。传入 `?cascade_private_children=true` 可级联删除私有子 Agent。返回删除作业句柄。

**`GET /api/control/agents/:id/delete-status`** — 查询删除作业状态

返回 Agent 删除作业的生命周期进度。

**`POST /api/control/agents/:id/timers`** — 创建定时器

为该 Agent 创建持久的延时或周期性定时器。

```json
{ "after_ms": 60000, "every_ms": null, "summary": "心跳检查" }
```

**`POST /api/control/agents/:id/timers/:timer_id/cancel`** — 取消定时器

按 id 取消活跃的定时器。

**`POST /api/control/agents/:id/tasks`** — 创建命令任务

为该 agent 启动一个后台命令任务。

```json
{
  "summary": "Build project",
  "cmd": "cargo build",
  "workdir": null,
  "shell": null,
  "login": false
}
```

**`POST /api/control/agents/:id/tasks/:task_id/input`** — 发送任务输入

以可信 operator 权威向受管任务投递文本输入。命令任务在创建时启用了交互输入的情况下
接收 stdin 或 TTY 文本；受监督的子 agent 任务接收一条后续输入。

```json
{ "text": "continue\n", "authority_class": "operator_instruction" }
```

**`POST /api/control/agents/:id/tasks/:task_id/stop`** — 停止任务

为受管任务请求取消，并返回结构化的任务停止回执。

```json
{ "authority_class": "operator_instruction" }
```

**`POST /api/control/agents/:id/work-items`** — 创建工作项

为该 agent 创建一个持久工作项。

```json
{
  "objective": "Fix the build",
  "authority_class": "operator_instruction"
}
```

**`POST /api/control/agents/:id/work-items/:work_item_id/pick`** — 选定工作项

把一个已有的打开工作项设为该 agent 的当前焦点。响应会返回先前焦点、当前焦点、
当前工作项 id 以及记录下来的焦点转换。

```json
{ "reason": "external scheduler selected next work", "authority_class": "integration_signal" }
```

**`PATCH /api/control/agents/:id/work-items/:work_item_id`** — 更新工作项

修改一个或多个 WorkItem 字段。空更新会被拒绝。`blocked_by`
使用嵌套的可选结构：字符串设置阻塞项，`null` 清除它，省略该字段则保持不变。
`recheck_after` 以毫秒为单位，且要求非空阻塞项。

```json
{
  "objective": "Fix the build and update docs",
  "plan_status": "ready",
  "todo_list": [{ "text": "Run cargo check", "state": "completed" }],
  "blocked_by": "waiting for CI",
  "recheck_after": 600000,
  "authority_class": "operator_instruction"
}
```

**`POST /api/control/agents/:id/work-items/:work_item_id/complete`** — 完成工作项

对于一个打开的工作项，原子性地把所提供的报告持久化为规范结果简报，绑定完成意图，
应用完成调度的副作用，并返回处于 `completed` 状态的 `WorkItemRecord`。同一端点也会
终结一个已有的旧版 `completing` 记录。空报告、取消、未完成关闭和删除都刻意不在该
生命周期接口的范围内。

```json
{
  "report_text": "Build fixed and all checks passed.",
  "authority_class": "operator_instruction"
}
```

**`POST /api/control/agents/:id/timers`** — 创建 timer

创建一个会向 agent 投递 `TimerTick` 的 timer。

```json
{
  "duration_ms": 60000,
  "interval_ms": null,
  "summary": "reminder",
  "authority_class": "operator_instruction"
}
```

**`POST /api/control/agents/:id/timers/:timer_id/cancel`** — 取消 timer

取消一个活跃 timer 并返回更新后的 `TimerRecord`。对已取消的 timer，取消是幂等的。
找不到 timer 时返回共享的 404 错误信封；已完成的 timer 返回共享的 400 生命周期错误，
因为它已经触发过。取消会立即更新 timer 列表/详情投影，并发出 `timer_cancelled`。

```json
{ "authority_class": "operator_instruction" }
```

**`POST /api/control/agents/:id/debug-prompt`** — 调试提示词

发送一个调试模式提示词（运行时内部分类）。请求体：

```json
{ "text": "debug instruction", "authority_class": "operator_instruction" }
```

**`POST /api/control/agents/:id/operator-bindings`** — 创建 operator 传输绑定

为 operator 通知设置回调 URL 或传输绑定。
请求体：

```json
{
  "binding_id": "my-binding",
  "transport": "http-callback",
  "operator_actor_id": "operator-1",
  "default_route_id": "default",
  "delivery_callback_url": "https://example.com/callback",
  "delivery_auth": { "type": "bearer", "token": "secret" },
  "capabilities": { "send_prompt": true },
  "provider": "anthropic",
  "provider_identity_ref": "user-123",
  "metadata": {}
}
```

**`POST /api/control/agents/:id/operator-ingress`** — Operator 入口

通过控制平面为 operator 来源消息提供的直接入口路径。
请求体：

```json
{
  "text": "operator message",
  "actor_id": "operator-1",
  "binding_id": "my-binding",
  "reply_route_id": "route-1",
  "provider": "anthropic",
  "correlation_id": "corr-123"
}
```

**`POST /api/control/agents/:id/workspace/attach`** — 附加 workspace

```json
{ "path": "/path/to/workspace" }
```

**`POST /api/control/agents/:id/workspace/exit`** — 退出当前 workspace

返回 agent home workspace。接受一个可选请求体：

```json
{ "authority_class": "operator_instruction" }
```

**`POST /api/control/agents/:id/workspace/detach`** — 分离 workspace

移除一个 workspace 注册而不切换。请求体：

```json
{ "workspace_id": "ws-abc123", "authority_class": "operator_instruction" }
```

**`POST /api/control/agents/:id/model`** — 设置 agent 模型

```json
{ "model": "claude-sonnet-4-20250514" }
```

**`POST /api/control/agents/:id/model/clear`** — 清除模型覆盖

恢复到默认模型。接受一个可选请求体：

```json
{ "authority_class": "operator_instruction" }
```

### 运行时管理

**`GET /api/control/runtime/status`** — 运行时状态

返回 daemon 和运行时健康信息，包括已配置的模型、control token 状态和活动标记。

**`GET /api/control/runtime/config`** — 运行时配置

返回 daemon 当前生效的运行时配置面，以及支撑持久化可变运行时设置的
`config.json` 路径。

**`PATCH /api/control/runtime/config`** — 更新运行时配置

持久化可变的运行时配置键更新，并对每次尝试的更改进行分类。当前实现会把受支持的键写入
`config.json` 并返回 `accepted_requires_restart`，因为在实时重载支持出现之前，
运行中的宿主会保持当前生效的配置。仅启动时可用或不受支持的键会返回带原因的 `rejected`。

```json
{
  "updates": [
    { "key": "model.default", "value": "openai/gpt-4.1" },
    { "key": "home_dir", "value": "/tmp/other-home" }
  ]
}
```

```json
{
  "ok": true,
  "changed": true,
  "results": [
    {
      "key": "model.default",
      "effect": "accepted_requires_restart",
      "reason": "persisted in config.json; the running host keeps its current effective config until restart/reload support is added"
    },
    {
      "key": "home_dir",
      "effect": "rejected",
      "reason": "unsupported or startup-only config key"
    }
  ],
  "runtime_surface": { "...": "..." }
}
```

**`POST /api/control/runtime/shutdown`** — 优雅关闭

优雅地关闭运行时和 daemon。

**`GET /api/control/runtime/metrics`** — 性能诊断快照

以标准 OpenMetrics 文本格式（`application/openmetrics-text; version=1.0.0; charset=utf-8`）返回运行时性能指标。适用于 Prometheus 抓取和运维监控。

**`GET /api/control/runtime/traces`** — 近期链路摘要

返回近期运行时 Trace 活动和 Span 摘要，便于延迟诊断。

### 工作区与桌面集成

**`POST /api/file-references/resolve`** — 批量解析文件引用

批量将最多 64 个文件引用（`workspace_uri`、`absolute_path` 或相对于已知基准文件的 `relative_path`）解析为规范的工作区文件定位与元数据。需远程访问授权。

请求体示例：

```json
{
  "references": [
    {
      "type": "workspace_uri",
      "workspace_uri": "workspace://ws_f375c191f64f3dd/src/main.rs"
    },
    {
      "type": "absolute_path",
      "absolute_path": "/home/user/project/README.md"
    }
  ]
}
```

响应体示例：

```json
{
  "results": [
    {
      "status": "resolved",
      "location": {
        "workspace_id": "ws_f375c191f64f3dd",
        "execution_root_id": "root_c8010df5",
        "path": "src/main.rs",
        "absolute_path": "/home/user/project/src/main.rs",
        "kind": "file",
        "root_kind": "canonical"
      }
    },
    {
      "status": "unresolved",
      "reason": "unknown_workspace",
      "message": "workspace not found"
    }
  ]
}
```

**`GET /api/desktop/capabilities`** — 桌面集成能力查询

返回当前连接是否支持桌面集成（如 macOS Finder 定位展示）。需服务端显式开启 `--desktop-integration`，运行在 macOS 上，且通过同源环回地址（loopback）直连访问。

```json
{
  "reveal_in_finder": true
}
```

**`POST /api/desktop/reveal`** — 在桌面文件管理器中展示文件

调用系统原生能力在 macOS Finder 中高亮选定文件或目录。必须在启用桌面集成、环回连接校验通过且目标路径严格受限于注册的工作区执行根内时才允许执行。

```json
{
  "workspace_id": "ws_f375c191f64f3dd",
  "execution_root_id": "root_c8010df5",
  "path": "src/main.rs"
}
```

### Webhook 与回调

**`POST /api/webhooks/generic/:agent_id`** — 通用 webhook

接受任意 JSON payload，并将其作为 `WebhookEvent` 消息入队到指定 agent。
适用于 GitHub webhook、CI 通知和外部服务集成。

**`POST /api/callbacks/enqueue/:callback_token`** — 回调 enqueue

接收来自已注册回调 URL 的 enqueue 回调。请求体上限：256 KB。

**`POST /api/callbacks/wake/:callback_token`** — 回调 wake

接收来自已注册回调 URL 的 wake 回调。

## 消息结构

### MessageKind

外部调用方有效的 enqueue 类型：`channel_event`、`webhook_event`。
策略只允许带 operator 来源的 `operator_prompt`；公共
enqueue 会拒绝它。运行时自有类型（`system_tick`、`task_result`、
`task_status`、`control`、`internal_followup`）会被外部 enqueue 拒绝。

### Priority

| 值 | 行为 |
|-------|----------|
| `interject` | 抢占普通队列；仅限控制平面 |
| `next` | 在当前 turn 之后、已排队内容之前 |
| `normal` | 标准队列位置 |
| `background` | 低优先级，空闲时处理 |

### TrustLevel

| 值 | 默认来源 | 含义 |
|-------|----------------|---------|
| `trusted_operator` | `operator` | 直接的 operator 操作 |
| `trusted_system` | `system`、`task`、`timer` | 运行时内部操作 |
| `trusted_integration` | `webhook`、`callback` | 带显式信任的已知集成 |
| `untrusted_external` | `channel` | 公共 channel / 未认证调用方 |

### MessageOrigin

| 类型 | 字段 |
|------|--------|
| `operator` | `actor_id`（可选） |
| `channel` | `channel_id`、`sender_id`（可选） |
| `webhook` | `source`、`event_type`（可选） |
| `callback` | `descriptor_id`、`source`（可选） |
| `timer` | `timer_id` |
| `system` | `subsystem` |
| `task` | `task_id` |

### MessageBody

| 类型 | 字段 |
|------|--------|
| `text` | `text: string` |
| `json` | `value: object` |
| `brief` | `title`、`text`、`attachments` |

## 设计目标

- 把传输细节排除在核心运行时模型之外。
- 为入站消息和外部事件保留来源信息。
- 返回结构化的生命周期状态，而不只是流式文本。
- 让 wake、sleep、enqueue 和任务监督对集成可见。
- 让面向用户的输出与内部追踪保持分离。

## 集成姿态

把 HTTP 接口当作运行时状态的控制平面，而不是仅用于聊天的端点。好的集成应当能够回答：

- 哪些工作处于活跃状态？
- 哪些任务正在运行或等待？
- 是什么事件唤醒了 agent？
- 哪些输出可以安全地展示给用户？
- 哪些证据是内部运行时细节？

### 尚未文档化的路由

以下路由存在于 [`src/http/mod.rs`](../../../src/http/mod.rs)，但尚未在本参考页完整记录：

- `GET /api/agents/:id/skills`
- `POST /api/control/agents/:id/skills/install`
- `POST /api/control/agents/:id/skills/uninstall`
- 默认 agent 别名：`/api/status`、`/api/briefs`、`/api/state`、`/api/transcript`、
  `/api/worktree-summary`

随着接口稳定，这些内容会陆续补充。

## 常用 curl 示例

```bash
# 检查服务端健康状态
curl http://127.0.0.1:7878/api/handshake

# 列出 agent
curl http://127.0.0.1:7878/api/agents/list

# 获取 agent 状态
curl http://127.0.0.1:7878/api/agents/main/state

# 发送提示词（需要 control token）
curl -X POST http://127.0.0.1:7878/api/control/agents/main/prompt \
  -H "Authorization: Bearer $HOLON_CONTROL_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"text": "Run cargo check"}'

# 通过 webhook 入队（公共）
curl -X POST http://127.0.0.1:7878/api/webhooks/generic/main \
  -H "Content-Type: application/json" \
  -d '{"event": "ci-complete", "status": "success"}'

# 流式获取 agent 事件
curl -N http://127.0.0.1:7878/api/agents/main/events/stream
```

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
