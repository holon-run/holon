---
title: API 契约清单
summary: Holon HTTP 控制平面 API 参数、响应和第二阶段契约工作的基线后稳定性清单。
order: 25
---

# API 契约清单

本页是 Holon **HTTP 控制平面 API** 的基线后清单。它补充
[HTTP 控制平面](/zh-CN/reference/http-control-plane.md)参考页，记录路由列表、
请求参数、响应形状，以及在脚本和集成能够长期依赖该 API 之前仍需稳定化的契约
缺口。

- **最后核对基准：** `holon` v0.39.0，`main` 位于 `885367b9`。
- **权威来源：** [`src/http/mod.rs`](../../../src/http/mod.rs) 的 Axum 路由和
  请求/响应结构体。
- **生成的 schema：** [`openapi.json`](/reference/openapi.json)，由
  `holon::openapi::generate_openapi_json()` 生成，并由 `make snapshots-check`
  检查。
- **路由/schema 漂移检查：** `tests/snapshots/http_route_inventory.json`，由
  `make snapshots-check` 从 Axum 路由树和生成的 OpenAPI 基线产出。
- **客户端源码：** `src/client.rs`，对应 TUI/CLI 使用的那部分子集。
- **当前状态：** 1.0 之前的基线。把下面的形状视为已观察到的行为，而不是最终的
  兼容性承诺。里程碑 8 确立了首个受检基线；剩余的缺口属于第二阶段稳定化工作。

## 稳定性级别

| 级别 | 含义 |
|-------|---------|
| 候选稳定 | 对公开使用有价值，且已记录或被 CLI/TUI 使用，但仍需要快照测试或显式的 schema 保证。 |
| 实验性 | 在当前运行时可用，但随运行时模型演进很可能变化。 |
| 能力 | 通过生成的回调能力令牌访问。不要暴露或记录令牌。 |
| 内部/调试 | 面向本地诊断、兼容别名或临时集成路径。 |
| 缺口 | 缺失或说明不足的 API 接口，在当作稳定接口之前应先设计。 |

## 跨领域契约

### 传输与认证

| 接口 | 当前行为 | 稳定性 |
|---------|------------------|-----------|
| HTTP TCP | 路由由 [`src/http/mod.rs`](../../../src/http/mod.rs) 中的 Axum 路由提供服务。 | 候选稳定 |
| Unix socket 客户端回退 | 存在配置的 Unix socket 时，`LocalClient` 先尝试它，再回退到 HTTP。 | 对本地客户端为候选稳定；未记录为通用的远程 API |
| Bearer 认证 | 当 `require_control_token` 为 true 时，读取路由和 `/api/control/*` 路由要求 `Authorization: Bearer <token>`。 | 候选稳定 |
| 本地模式 | 不需要 control token 时，服务端信任本地进程边界。 | 候选稳定，但应绑定到明确的部署指引 |
| 回调能力令牌 | `/api/callbacks/*/:callback_token` 路由通过把令牌解析为外部触发器记录来认证。 | 能力 |

### 入口信任/认证边界

| 入口类别 | 路由 | 认证边界 | 运行时来源 | 优先级策略 | 稳定性 |
|---------------|--------|---------------|--------------------|-----------------|-----------|
| 公共 enqueue | `/api/enqueue`、`/api/agents/:agent_id/enqueue` | bearer 模式下用 Bearer token；local 模式下用本地进程边界。 | 只接受 channel 或 webhook 来源。调用方提供的 `trust` 会被拒绝。channel 来源成为不可信的外部证据；webhook 来源成为集成信号。运行时自有的消息类型会被拒绝。 | 允许 `next`、`normal` 和 `background`；拒绝 `interject`。 | 候选稳定 |
| 回调能力 | `/api/callbacks/wake/:callback_token`、`/api/callbacks/enqueue/:callback_token` | 路径中的能力令牌必须解析为一个活跃的外部触发器，并与路由的投递模式匹配。完整回调 URL 属于机密。 | 作为外部触发器能力和集成信号被接纳。wake 模式发出运行时自有的检查 tick，而不是把 payload 当作 operator 指令信任。 | 调用方不选择队列优先级。 | 能力 |
| Operator 传输绑定 | `/api/control/agents/:agent_id/operator-bindings` | 控制平面认证。输入的投递 Bearer token 会被校验，并在审计事件中脱敏。 | 记录远程 operator 入口和投递回调所使用的绑定。 | 不适用。 | 实验性 |
| Operator 传输入口 | `/api/control/agents/:agent_id/operator-ingress` | 控制平面认证，外加活跃绑定、匹配的 agent、匹配的 actor，以及（在提供时）匹配的 provider。 | 入队一条带 `operator_instruction` 权威和远程 operator 传输元数据的可信 operator 提示。 | 始终为 `interject`。 | 实验性 |
| 通用 webhook 兼容 | `/api/webhooks/generic/:agent_id` | bearer 模式下用 Bearer token；local 模式下用本地进程边界。 | 把 JSON payload 转换为来自 `generic_webhook` 的可信集成 webhook 事件；整个 body 就是 payload，因此路由忽略调用方提供的来源信息。 | 始终为 `normal`。 | 内部/调试 |

### 通用 JSON 与响应行为

大多数成功响应是 JSON，但 Holon 不采用单一全局成功信封。稳定策略按路由类别划分：

| 路由类别 | 成功策略 | 理由 |
|-------------|----------------|-----------|
| 发现 | 信封响应包含 `ok: true` 和发现字段，但 `/api/models` 等目录式发现路由有意返回直接记录。 | 小型发现握手受益于显式的存活标记；目录记录应保持可直接消费。 |
| 读模型 | 直接返回记录或数组，不带合成的 `ok` 字段。 | 读取路由暴露已有的运行时记录和列表；加一层包装信封只会让 CLI/TUI 消费者多做一次解包，却不增加状态转换信息。 |
| 控制变更 | 信封响应包含 `ok: true` 和变更结果字段，除非端点创建并返回一等记录。 | 变更调用方需要清晰的接纳/副作用确认；创建记录的路由可以用创建出的记录作为确认。 |
| 流 | 服务端发送事件（SSE）帧，事件数据为 JSON；流打开后没有 JSON 成功信封。 | 成功的响应就是流本身。游标和事件信封细节由事件/SSE 契约覆盖。 |
| 能力回调 | 信封响应包含 `ok: true` 和回调投递结果字段。 | 回调调用方需要紧凑的确认，同时保留能力特有的投递结果。 |

当前代表性示例：

- `/api/` 和 `/api/handshake` 返回 `{ "ok": true, ... }`。
- `/api/models` 返回 `{ "available_models": ..., "model_availability": ... }`，
  没有 `ok` 字段。
- `/api/agents/list`、`/api/agents/:id/status`、`/api/agents/:id/state`、
  `/api/agents/:id/tasks` 等读取路由直接返回记录或数组。
- `/api/control/agents/:id/prompt`、`/api/control/agents/:id/wake` 以及
  workspace/model 变更路由返回 `{ "ok": true, ... }`。
- `/api/control/agents/:id/tasks`、`/api/control/agents/:id/work-items` 和
  `/api/control/agents/:id/timers` 直接返回创建出的记录。
- `/api/agents/:id/events/stream` 在流打开后返回服务端发送事件，而不是 JSON
  响应体。

处理器产生的控制平面错误使用一个共享 JSON 信封：

```json
{
  "ok": false,
  "error": "message",
  "code": "machine_readable_code",
  "hint": "optional operator guidance"
}
```

对这些处理器产生的错误，`ok` 始终为 `false`；`error` 是人类可读消息。`code`
和 `hint` 是可选共享字段。路由可以在共享字段之外添加有文档的路由专属扩展字段。
在处理器运行前产生的框架级拒绝，例如 Axum 针对畸形 JSON 的 extractor 失败，或
被 `DefaultBodyLimit` 拒绝的请求体，尚未纳入该信封。

状态码映射：

| 状态 | 类别 | 当前映射 |
|--------|-------|-----------------|
| `400 Bad Request` | 校验 | 畸形或不支持的请求字段、必填空字符串、无效的回调 body、不支持的 operator 投递认证。 |
| `403 Forbidden` | 认证/授权 | 缺失、畸形或无效的 bearer token；私有 agent 访问；无效的回调能力令牌；入口策略拒绝。 |
| `404 Not Found` | 资源/游标缺失 | 未知的公共 agent、已归档的公共 agent、未知的兼容路由，或事件游标落在重放窗口之外。 |
| `409 Conflict` | 状态冲突 | agent 已停止、abort 的当前 run 过期或缺失、重复安装 skill、以冲突形式出现的活跃工作区 detach 冲突。 |
| `424 Failed Dependency` | 依赖不可用 | skill manager 不可用。 |
| `502 Bad Gateway` | 上游失败 | 远程 skill 安装器失败。 |
| `504 Gateway Timeout` | 上游超时 | 远程 skill 安装器超时。 |
| `503 Service Unavailable` | 运行时服务不可用 | 需要运行时服务元数据但它缺失。 |
| `500 Internal Server Error` | 内部/运行时错误 | 意外的运行时、存储、工作区或处理器错误。 |

当前常见的路由专属错误扩展包括：

| 字段 | 使用者 | 含义 |
|-------|---------|---------|
| `agent_id` | 已停止 agent 和无当前 run 冲突 | 与被拒操作相关的 agent。 |
| `after_seq` / `event_seq` | 事件分页/SSE 游标错误 | 在重放窗口中不可用的游标序列。 |
| `requested_run_id` / `current_run_id` | 当前 run abort 冲突 | 过期的请求 run 和当前活跃 run。 |
| `skill_name`、`destination`、`manager`、`package`、`exit_status`、`stdout`、`stderr`、`timeout_seconds` | skill 安装错误 | skill manager 或远程安装器的诊断。 |

已知的稳定错误 `code` 值：

| Code | 状态 | 含义 |
|------|--------|---------|
| `agent_stopped` | `409` | 目标 agent 已停止，在 prompt 或 wake 前必须先启动。 |
| `cursor_not_found` | `404` | 请求的事件游标落在保留的重放窗口之外。 |
| `stale_run_id` | `409` | abort 请求指定的 run 已不再是当前 run。 |
| `no_current_run` | `409` | abort 请求找不到可中止的活跃 run。 |
| `skill_already_installed` | `409` | skill 目标位置已存在。 |
| `skill_manager_unavailable` | `424` | 所需的 skill manager 可执行文件不可用。 |
| `remote_skill_install_failed` | `502` | 远程 skill 安装器以失败状态退出。 |
| `remote_skill_install_timeout` | `504` | 远程 skill 安装器超时。 |

`src/client.rs` 以兼容方式解码该信封：显示时只要求 `error`，并在
`LocalHttpError` 中保留可选的 `code` 和 `hint`。除非调用方直接检查原始响应，
否则客户端会忽略未知扩展字段。

## 端点清单

### 发现与运行时

| 方法 | 路径 | 输入 | 成功响应 | 稳定性 | 备注 |
|--------|------|--------|------------------|-----------|-------|
| `GET` | `/api/` | bearer 模式下的认证头。 | `{ ok, default_agent }` | 候选稳定 | 用于发现默认 agent id 的根路由。 |
| `GET` | `/api/handshake` | bearer 模式下的认证头。 | `{ ok, protocol, auth, capabilities, runtime }` | 候选稳定 | 协议版本当前为 `holon-control` / `1`。 |
| `GET` | `/api/models` | bearer 模式下的认证头。 | `{ available_models, model_availability }` | 实验性 | 响应没有 `ok` 信封，返回模型目录/可用性内部信息。 |
| `GET` | `/api/control/runtime/readiness` | 控制平面认证。 | 类似 `RuntimeStatusResponse` 的 readiness payload。 | 候选稳定 | 供守护进程/客户端做就绪检查。 |
| `GET` | `/api/control/runtime/status` | 控制平面认证。 | 带活动、启动接口、运行时配置接口和最后失败的 `RuntimeStatusResponse`。 | 候选稳定 | 响应可能暴露运行时配置摘要；保持凭据字段脱敏。 |
| `POST` | `/api/control/runtime/shutdown` | 控制平面认证；客户端中 body 被忽略/为空 JSON。 | `RuntimeShutdownResponse` | 实验性 | 生命周期控制；应保持关闭语义明确。 |

### Agent 读模型

| 方法 | 路径 | 输入 | 成功响应 | 稳定性 | 备注 |
|--------|------|--------|------------------|-----------|-------|
| `GET` | `/api/agents/list` | bearer 模式下的认证头。 | `AgentListEntry[]` | 候选稳定 | 供选择/导航的轻量列表。 |
| `GET` | `/api/agents/:agent_id/status` | 路径 `agent_id`；bearer 模式下的认证头。 | `AgentSummary` | 候选稳定 | 单个 agent 的主要读模型。 |
| `GET` | `/api/agents/:agent_id/state` | 路径 `agent_id`；bearer 模式下的认证头。 | `AgentStateSnapshot` | 实验性 | 轻量引导快照；省略 operator 通知、重复执行细节、任务细节和完整工作项内部信息。 |
| `GET` | `/api/agents/:agent_id/briefs` | 路径 `agent_id`；查询 `limit?`。 | `BriefRecord[]` | 候选稳定 | 默认 `20`。 |
| `GET` | `/api/agents/:agent_id/tasks` | 路径 `agent_id`；查询 `limit?`。 | `TaskRecord[]` | 列表为候选稳定；DTO schema 仍然宽泛 | 默认 `50`；列出活跃/近期任务。 |
| `GET` | `/api/agents/:agent_id/tasks/:task_id` | 路径 `agent_id`、`task_id`。 | `TaskStatusSnapshot` | 路由为候选稳定；DTO schema 仍然宽泛 | 返回单个任务的生命周期快照。 |
| `GET` | `/api/agents/:agent_id/tasks/:task_id/output` | 路径 `agent_id`、`task_id`；查询 `block?`、`timeout_ms?`。 | `TaskOutputResult` | 路由为候选稳定；DTO schema 仍然宽泛 | 读取有界的任务输出，可选等待就绪。 |
| `GET` | `/api/agents/:agent_id/timers` | 路径 `agent_id`；查询 `limit?`。 | `TimerRecord[]` | 候选稳定 | 默认 `50`。 |
| `GET` | `/api/agents/:agent_id/transcript` | 路径 `agent_id`；查询 `limit?`。 | `TranscriptEntry[]` | 实验性 | transcript 数据可能包含 provider/工具内部信息。 |
| `GET` | `/api/agents/:agent_id/worktree-summary` | 路径 `agent_id`。 | `{ agent_id, summary }` | 实验性 | summary 形状跟随受管理工作区的内部结构。 |
| `GET` | `/api/agents/:agent_id/skills` | 路径 `agent_id`；bearer 模式下的认证头。 | `{ ok, agent_id, skills }` | 实验性 | skills 列表形状跟随本地 skill 目录记录。 |
| `GET` | `/api/agents/:agent_id/skills/:skill_id` | 路径 `agent_id`、`skill_id`；bearer 模式下的认证头。 | `{ ok, skill, content }` | 实验性 | 从目标 agent 的有效目录和活跃 execution root 解析详情。 |

默认 agent 别名：

| 方法 | 路径 | 别名目标 | 稳定性 |
|--------|------|--------------|-----------|
| `GET` | `/api/status` | `/api/agents/:default/status` | 内部/调试兼容 |
| `GET` | `/api/briefs` | `/api/agents/:default/briefs` | 内部/调试兼容 |
| `GET` | `/api/state` | `/api/agents/:default/state` | 内部/调试兼容 |
| `GET` | `/api/transcript` | `/api/agents/:default/transcript` | 内部/调试兼容 |
| `GET` | `/api/worktree-summary` | `/api/agents/:default/worktree-summary` | 内部/调试兼容 |

### 事件与流

| 方法 | 路径 | 输入 | 成功响应 | 稳定性 | 备注 |
|--------|------|--------|------------------|-----------|-------|
| `GET` | `/api/agents/:agent_id/events` | 路径 `agent_id`；查询 `before_seq?`、`after_seq?`、`limit?`、`order?`、`max_level?`。 | `EventsPageResponse` | 路由和信封为候选稳定 | `limit` 默认为事件窗口，并被钳制。`order` 为 `asc` 或 `desc`。`max_level` 只过滤事件是否被包含。 |
| `GET` | `/api/agents/:agent_id/events/stream` | 路径 `agent_id`；查询 `after_seq?`、`limit?`；建议 `Accept: text/event-stream`。 | 带 JSON `StreamEventEnvelope` 数据的 SSE 帧。 | 路由和信封为候选稳定 | SSE `id` 是 `event_seq`；SSE `event` 是原始审计事件类型。 |

事件分页游标是排他的：`after_seq` 返回 `event_seq` 更大的记录，`before_seq`
返回 `event_seq` 更小的记录，两者同时给出时返回
`after_seq < event_seq < before_seq`。省略 `after_seq` 时，SSE 流从当前尾部
之后开始；`after_seq=0` 时从当前重放窗口重放；当非零 `after_seq` 落在该重放
窗口之外时，会在打开流之前返回 `404 cursor_not_found`。落后的实时接收方会关闭
SSE 连接，而不是跨越隐藏的间隙继续；客户端从每个 agent 最后一个连续的
`event_seq` 恢复。仅实时的全局流没有全局游标，因此客户端要分别回填每个 agent。

稳定的 `StreamEventEnvelope` 字段：

```json
{
  "id": "event-uuid",
  "event_seq": 42,
  "ts": "2026-05-24T00:00:00Z",
  "agent_id": "main",
  "type": "task_created",
  "provenance": { "authority_class": "operator_instruction", "task_id": "task-..." },
  "payload": {}
}
```

事件 payload 是协议标准，会完整包含。事件页可以用
`max_level=info|verbose|debug` 过滤事件是否被包含；过滤不会改变 `payload`。
实时事件流是原始的，不支持级别过滤。

从已移除的 projection 契约迁移的破坏性变更：

- 从事件分页和流请求中删除 `projection=operator` / `projection=local_debug`
- 停止读取 `StreamEventEnvelope.projection`；现在所有信封都包含完整的标准 payload
- 用 `/api/agents/:id/events?max_level=info` 获取 operator 密度的历史页面，
  原始流则由客户端侧过滤

### 公共入口

| 方法 | 路径 | 输入 | 成功响应 | 稳定性 | 备注 |
|--------|------|--------|------------------|-----------|-------|
| `POST` | `/api/enqueue` | `EnqueueRequest`；bearer 模式下的认证头。 | `{ ok, agent_id, message_id }` | 候选稳定 | 入队到默认 agent。 |
| `POST` | `/api/agents/:agent_id/enqueue` | 路径 `agent_id`；`EnqueueRequest`；bearer 模式下的认证头。 | `{ ok, agent_id, message_id }` | 候选稳定 | 公共调用方不得设置 `trust` 或 `interject` 优先级。 |
| `POST` | `/api/webhooks/generic/:agent_id` | 路径 `agent_id`；JSON payload；bearer 模式下的认证头。 | `{ ok, agent_id, message_id }` | 内部/调试 | 把 payload 转换为可信集成 webhook 事件的兼容/调试路由。新集成应优先使用公共 enqueue 或回调能力。 |

`EnqueueRequest` 字段：

| 字段 | 类型/取值 | 必填 | 备注 |
|-------|---------------|----------|-------|
| `kind` | `channel_event`、`webhook_event` 等 | 否 | 默认为 `webhook_event`。`system_tick`、`callback_event` 等运行时自有类型会被拒绝。 |
| `priority` | `next`、`normal`、`background`；仅可信入口可用 `interject` | 否 | 默认为 `normal`；公共 enqueue 拒绝 `interject`。 |
| `trust` | `trusted_operator`、`trusted_system`、`trusted_integration`、`untrusted_external` | 否 | 公共 enqueue 拒绝调用方提供的 trust。 |
| `body` | `MessageBody` | 否 | 存在时按原样使用。 |
| `text` | string | 否 | `body` 缺失时转换为 text body。 |
| `json` | JSON value | 否 | `body` 和 `text` 都缺失时转换为 JSON body。 |
| `metadata` | JSON value | 否 | 存储在消息上。 |
| `correlation_id` / `causation_id` | string | 否 | 透传到消息信封。 |
| `origin` | 公共 enqueue 下为 `channel` 或 `webhook` | 否 | 公共 enqueue 拒绝 operator、timer、system 和 task 来源。 |

### 控制操作

| 方法 | 路径 | 请求体 | 成功响应 | 稳定性 | 备注 |
|--------|------|--------------|------------------|-----------|-------|
| `POST` | `/api/control/agents/:agent_id/prompt` | `{ text }` | `{ ok, agent_id, message_id }` | 候选稳定 | 以 `interject` 优先级入队一条可信 operator 提示。 |
| `POST` | `/api/control/agents/:agent_id/wake` | `{ reason, source?, correlation_id?, causation_id? }` | `{ ok, agent_id, disposition }` | 候选稳定 | 空 `reason` 会被拒绝。不会启动已停止的 agent。 |
| `POST` | `/api/control/agents/:agent_id/control` | `{ action, authority_class? }`；`action` 为 `start` 或 `stop`。 | `{ ok }` | 候选稳定 | `authority_class` 当前仅为审计/来源元数据。 |
| `POST` | `/api/control/agents/:agent_id/current-run/abort` | `{ run_id?, mode?, authority_class? }` | `{ ok, aborted, agent_id, run_id, mode, admission_context, provided_trust }` | 候选稳定 | `mode` 默认为 `stop_after_abort`；接受已废弃别名 `pause_after_abort`。 |
| `POST` | `/api/control/agents/:agent_id/create` | `{ template?, authority_class? }` | `AgentSummary` | 实验性 | 路径 id 命名创建出的 agent。 |
| `POST` | `/api/control/agents/:agent_id/debug-prompt` | `{ text, authority_class? }` | `{ ok, agent_id, dump }` | 内部/调试 | 转储 prompt 渲染，不应作为稳定的自动化 API。 |

### 任务、工作项和定时器

| 方法 | 路径 | 请求体 | 成功响应 | 稳定性 | 备注 |
|--------|------|--------------|------------------|-----------|-------|
| `POST` | `/api/control/agents/:agent_id/tasks` | `CreateCommandTaskRequest` | `TaskRecord` | 创建为候选稳定；DTO schema 仍然宽泛 | `serde(deny_unknown_fields)` 拒绝旧字段。 |
| `POST` | `/api/control/agents/:agent_id/tasks/:task_id/input` | `{ text, authority_class? }` | `TaskInputResult` | 路由为候选稳定；DTO schema 仍然宽泛 | 把 operator 权威的文本投递给交互式命令任务或受监督的子 agent 任务。 |
| `POST` | `/api/control/agents/:agent_id/tasks/:task_id/stop` | `{ authority_class? }` | `TaskStopResult` | 路由为候选稳定；DTO schema 仍然宽泛 | 请求取消受管理任务。 |
| `GET` | `/api/agents/:agent_id/work-items` | 无 | `WorkItemRecord[]` | 实验性读模型；CLI schema 所有者 | 查询参数：`limit`；供 `holon work-item list` 使用。 |
| `GET` | `/api/agents/:agent_id/work-items/:work_item_id` | 无 | `WorkItemRecord` | 实验性读模型；CLI schema 所有者 | 供 `holon work-item get` 使用；目标 agent 下找不到该 id 时返回 404。 |
| `POST` | `/api/control/agents/:agent_id/work-items` | `{ objective, authority_class? }` | `WorkItemRecord` | 实验性 | 创建/入队工作项。 |
| `POST` | `/api/control/agents/:agent_id/work-items/:work_item_id/pick` | `{ reason?, clear_blocker?, authority_class? }` | `PickWorkItemResponse` | 实验性 | 设置当前 WorkItem 焦点；当 `clear_blocker=true` 且提供 `reason` 时，可以显式清除已解决的 blocker。 |
| `PATCH` | `/api/control/agents/:agent_id/work-items/:work_item_id` | `UpdateWorkItemRequest` | `WorkItemRecord` | 实验性 | 变更 objective、plan status、todo list 以及旧版 blocker/recheck 字段。空变更会被拒绝。 |
| `POST` | `/api/control/agents/:agent_id/work-items/:work_item_id/complete` | `{ report_text, authority_class? }` | `WorkItemRecord` | 实验性 | 以 `report_text` 作为规范结果 brief，原子地完成一个 open WorkItem；也会终结已有的旧版 `completing` 记录。取消/删除仍不在范围内。 |
| `GET` | `/api/agents/:agent_id/timers/:timer_id` | 路径 `agent_id`、`timer_id`。 | `TimerRecord` | 路由为候选稳定；DTO schema 仍然宽泛 | 目标 agent 下找不到该 timer id 时返回 404。 |
| `POST` | `/api/control/agents/:agent_id/timers` | `{ duration_ms, interval_ms?, summary?, authority_class? }` | `TimerRecord` | 候选稳定 | `duration_ms` 必填；`interval_ms` 使其成为重复定时器。 |
| `POST` | `/api/control/agents/:agent_id/timers/:timer_id/cancel` | `{ authority_class? }` | `TimerRecord` | 候选稳定 | 对已取消的定时器幂等。定时器缺失返回 404；已完成的定时器返回 400，因为它已经触发过。 |

`CreateCommandTaskRequest` 字段：

| 字段 | 类型 | 必填 | 默认值/备注 |
|-------|------|----------|-----------------|
| `summary` | string | 是 | 任务摘要。 |
| `cmd` | string | 是 | 传给命令任务运行器的命令行。 |
| `workdir` | string 或 null | 否 | 可选工作目录。 |
| `shell` | string 或 null | 否 | 可选 shell。 |
| `login` | bool | 否 | 默认为 `true`。 |
| `tty` | bool | 否 | 默认为 `false`。 |
| `yield_time_ms` | integer | 否 | 默认为 `10000`。 |
| `max_output_tokens` | integer 或 null | 否 | 有界输出预览预算。 |
| `accepts_input` | bool | 否 | 默认为 `false`。 |
| `authority_class` | `AuthorityClass` 或 null | 否 | 对已接纳的控制平面请求默认为 operator 权威；记录在来源/审计中。 |

### 工作区与模型控制

| 方法 | 路径 | 请求体 | 成功响应 | 稳定性 | 备注 |
|--------|------|--------------|------------------|-----------|-------|
| `POST` | `/api/control/agents/:agent_id/workspace/attach` | `{ path, authority_class? }` | `{ ok, agent_id, workspace_id, workspace_anchor }` | 候选稳定 | `path` 会转换为工作区条目。 |
| `POST` | `/api/control/agents/:agent_id/workspace/exit` | `{ authority_class? }` | `{ ok, agent_id }` | 候选稳定 | 让 agent 回到默认工作区行为。 |
| `POST` | `/api/control/agents/:agent_id/workspace/detach` | `{ workspace_id, authority_class? }` | `{ ok, agent_id, workspace_id }` | 候选稳定 | 使用前会去除 `workspace_id` 两端空白。 |
| `POST` | `/api/control/agents/:agent_id/model` | `{ model, reasoning_effort?, authority_class? }` | `{ ok, agent_id, model }` | 实验性 | `reasoning_effort` 会对照所选模型的目录元数据校验。Codex 模型可能暴露 `max`；在 Holon 实现其编排语义之前，`ultra` 不可用。 |
| `POST` | `/api/control/agents/:agent_id/model/clear` | `{ authority_class? }` | `{ ok, agent_id, model }` | 实验性 | 清除 agent 级模型覆盖。 |

### Operator 传输集成

| 方法 | 路径 | 请求体 | 成功响应 | 稳定性 | 备注 |
|--------|------|--------------|------------------|-----------|-------|
| `POST` | `/api/control/agents/:agent_id/operator-bindings` | `OperatorTransportBindingRequest` | `{ ok, agent_id, binding }` | 实验性 | 提供 `target_agent_id` 时必须与路由的 `agent_id` 匹配。 |
| `POST` | `/api/control/agents/:agent_id/operator-ingress` | `OperatorIngressRequest` | `{ ok, agent_id, message_id }` | 实验性 | 需要活跃绑定和匹配的 actor/provider。入队一条可信 operator 提示。 |

`OperatorTransportBindingRequest` 是严格的（`deny_unknown_fields`），包含
`binding_id?`、`transport`、`operator_actor_id`、`target_agent_id?`、
`default_route_id`、`delivery_callback_url`、`delivery_auth`、`capabilities`、
`provider?`、`provider_identity_ref?` 和 `metadata?`。

`delivery_auth.kind = "bearer"` 要求非空的 `bearer_token`。
在实现 HMAC 签名之前，`delivery_auth.kind = "hmac"` 会被拒绝。

### Skills

Skill 管理把库操作和 agent 启用分开：

| 方法 | 路径 | 请求体 | 成功响应 | 稳定性 | 备注 |
|--------|------|--------------|------------------|-----------|-------|
| `GET` | `/api/skills/catalog` | 无 | `{ catalog: [...] }` | 实验性 | 列出 Skill Library 中的所有 skill。 |
| `GET` | `/api/skills/catalog/:skill_id` | 无 | skill 详情对象 | 实验性 | 返回单个库 skill 的元数据。 |
| `POST` | `/api/skills/catalog/add` | `{ kind }`，其中 `kind` 是 `SkillInstallKind` 标签联合。 | `{ skill_name }` | 实验性 | 向库中添加一个 skill。错误映射为冲突、未找到或超时。 |
| `POST` | `/api/skills/catalog/remove` | `{ name }` | `{ skill_name }` | 实验性 | 从库中移除一个 skill。 |
| `POST` | `/api/skills/catalog/reconcile` | `{ name? }` | `{ ... }` | 实验性 | 让库与 `.skill-lock.json` 对账。 |
| `POST` | `/api/skills/catalog/refresh` | `{ }` | `{ ok, catalog }` | 实验性 | 通过重新扫描本地 skill 根目录刷新运行时目录。不与 lock 文件对账，也不拉取远程更新。 |
| `POST` | `/api/skills/catalog/check` | `{ name? }` | `{ ... }` | 实验性 | 检查库的一致性。 |
| `GET` | `/api/agents/:agent_id/skills` | 无 | `{ ok, agent_id, skills }` | 实验性 | 列出为某个 agent 启用的 skill。 |
| `GET` | `/api/agents/:agent_id/skills/:skill_id` | 无 | `{ ok, skill, content }` | 实验性 | 以目标 agent 的活跃 execution root 作为版本上下文，返回一个有效 skill。 |
| `POST` | `/api/control/agents/:agent_id/skills/enable` | `{ name, copy? }` | `{ ok, agent_id, skill_name }` | 实验性 | 为某个 agent 启用一个库 skill。 |
| `POST` | `/api/control/agents/:agent_id/skills/disable` | `{ name }` | `{ ok, agent_id, skill_name }` | 实验性 | 为某个 agent 禁用一个 skill。 |
| `POST` | `/api/control/agents/:agent_id/skills/install` | `{ kind }` | `{ ok, agent_id, skill_name }` | 已废弃 | add/enable 的兼容别名。 |
| `POST` | `/api/control/agents/:agent_id/skills/uninstall` | `{ name }` | `{ ok, agent_id, skill_name }` | 已废弃 | remove/disable 的兼容别名。 |

### 回调能力入口

| 方法 | 路径 | 输入 | 成功响应 | 稳定性 | 备注 |
|--------|------|--------|------------------|-----------|-------|
| `POST` | `/api/callbacks/enqueue/:callback_token` | 路径中的能力令牌；任意 body；`Content-Type` 指导 body 解码。 | `{ ok, ...CallbackDeliveryResult }` | 能力 | 令牌必须解析为投递模式为 enqueue 的活跃外部触发器。 |
| `POST` | `/api/callbacks/wake/:callback_token` | 路径中的能力令牌；任意 body；`Content-Type` 指导 body 解码。 | `{ ok, ...CallbackDeliveryResult }` | 能力 | 令牌必须解析为投递模式为 wake 的活跃外部触发器。 |

Body 解码规则：

- JSON content type 变成 `MessageBody::Json`。
- `text/*` content type 变成 `MessageBody::Text`。
- 其他带类型的 body 变成 JSON，附带 `content_type` 和 `body_base64`。
- 无类型的 UTF-8 body 变成 text；非 UTF-8 body 变成 JSON，附带 `body_base64`。

## 需要稳定化的共享记录形状

以下响应类型由多个端点暴露，应作为 schema 接口来对待，而不是偶然的 Rust 结构体：

| 形状 | 返回者 | 关键稳定性关注点 |
|-------|-------------|------------------------|
| `AgentSummary` | `/api/agents/:id/status`、`/api/agents/:id/state`、agent 创建 | 身份/profile 字段、status 枚举、模型状态、工作区字段。 |
| `AgentListEntry` | `/api/agents/list` | 保持轻量；避免重新引入沉重的运行时/模型 payload。 |
| `TaskRecord` | `/api/agents/:id/tasks`、任务创建、状态快照、事件 | 任务 kind/status 枚举、详情截断、恢复元数据、输出引用。 |
| `WorkItemRecord` | 工作项创建、状态快照、事件 | state、plan status、plan artifact、todo list、blocker/recheck 时间戳。 |
| `TimerRecord` | `/api/agents/:id/timers`、`/api/agents/:id/timers/:timer_id`、定时器创建、状态快照 | 重复定时器字段和 status 枚举。 |
| `BriefRecord` | `/api/agents/:id/briefs` | 面向用户的交付与内部轨迹。 |
| `TranscriptEntry` | `/api/agents/:id/transcript` | 可能包含 provider/工具内部信息和截断策略。 |
| `StreamEventEnvelope` | 事件分页和 SSE 流 | projection/脱敏、来源、payload 版本。 |
| `RuntimeStatusResponse` | 运行时 readiness/status | 启动/运行时配置接口和凭据脱敏。 |
| `SkillInstallKind` | skill 安装 | 标签联合变体和本地/远程包语义。 |

### v0.38.0 身份迁移遗留

`AgentVisibility`、`AgentOwnership`、`PrivateChild` 和 `PublicNamed` 仅作为
v0.38.0 迁移词汇保留。它们不是当前的公共 API 身份区分符。新集成应使用
`CreateAgent` 创建可寻址 agent，使用 `InvokeAgent` 进行父级监督的委派执行。

## 已发现的契约缺口

1. **OpenAPI 路由/类型元数据仍只部分与实现同处一地。** 路由覆盖和 schema 快照
   已经存在，但生成的基线仍依赖一份保守的 OpenAPI 表。第二阶段会把操作元数据
   移近 Axum 路由/类型定义。
2. **稳定 DTO schema 需要收紧。** 若干 task、work-item、timer、agent、event 和
   信封响应在 OpenAPI 基线中仍以宽泛形式表示；在它们成为稳定的客户端契约之处，
   应变成一等带类型组件。
3. **事件级别过滤需要更多真实场景覆盖。** 在把 `max_level` 的包含规则当作最终
   规则之前，应针对真实的 TUI/客户端会话进行验证。
4. **WorkItem 变更 API 现在覆盖 focus/update/complete。** HTTP 可以列出/读取/
   创建/入队工作项，拾取焦点，更新 objective/planning/blocker 字段，并完成工作项。
   在需要独立生命周期契约之前，取消/删除有意保持在范围外。
5. **定时器生命周期 API 现在覆盖取消。** HTTP 可以创建/列出/读取定时器，并取消
   活跃定时器。删除/清除仍不在范围内。
6. **部署指引仍需加固。** HTTP 入口信任/认证表已有文档，但面向生产的指引仍应
   说明何时使用 bearer 模式、本地模式、回调能力和专用 operator 适配器。
7. **工具 schema 是独立的 API 接口。** 内置工具的输入/结果 schema 现在有了自己
   的受检清单；本 HTTP 清单应链接到它，而不是重复它。

## 跟踪 issue

里程碑 8 的基线 issue 已完成。第二阶段的后续 issue 归在同一个
[`CLI/API Stability Contracts`](https://github.com/holon-run/holon/milestone/8)
里程碑下，并由
[#1444](https://github.com/holon-run/holon/issues/1444) 跟踪。

| Issue | 范围 |
|-------|-------|
| [#1438](https://github.com/holon-run/holon/issues/1438) `api: migrate OpenAPI baseline to aide route/type metadata` | 把 OpenAPI 操作元数据移近路由和 DTO 定义。 |
| [#1439](https://github.com/holon-run/holon/issues/1439) `api: tighten OpenAPI DTO schemas for stable read models` | 用带类型的稳定 DTO schema 替换选定的通用 JSON schema。 |
| [#1443](https://github.com/holon-run/holon/issues/1443) `events: define stable operator-facing event payload subset` | 为稳定事件字段定版本/写文档。事件 payload 是协议标准；`max_level` 只过滤事件是否被包含。 |

## 建议的后续工作

1. 把 OpenAPI 基线迁移到 `aide` 路由/类型元数据
   （[#1438](https://github.com/holon-run/holon/issues/1438)）。
2. 为稳定读模型和控制平面结果收紧 DTO schema
   （[#1439](https://github.com/holon-run/holon/issues/1439)）。
3. 定义稳定的、面向 operator 的事件 payload 子集
   （[#1443](https://github.com/holon-run/holon/issues/1443)）。
