---
title: Local App 参考
summary: 由 Agent 托管的应用清单字段、浏览器 SDK 方法、事件作用域与托管限制。
order: 85
---

<a id="local-app-reference"></a>

# Local App 参考

Local App 是保存在 Agent 的 `apps/<app-id>/` 目录下并通过 `/apps/<agent-id>/<app-id>/` 对外提供服务的静态 Web 应用。关于心智模型请参阅 [Local App 概念](../concepts/local-apps.md)，实操示例请参阅[托管 Local App](../guides/host-local-apps.md)。

<a id="manifest-contract"></a>

## 清单契约（Manifest contract）

每个应用必须在根目录下提供不超过 64 KiB 的 `manifest.json`。

| 字段 | 必填 | 契约约束 |
|---|---|---|
| `id` | 是 | 必须与应用目录名称以及请求的应用 ID 完全一致。 |
| `name` | 是 | 非空的展示名称。 |
| `version` | 是 | 非非空版本字符串。 |
| `entry` | 是 | 安全且非空的文件相对路径，指向应用根目录内的 HTML 文件。 |
| `description` | 否 | 简短描述。 |

服务端基于所属 Agent 的 Home 目录解析相对路径。客户端传递逻辑 Agent ID 与 App ID，而非宿主机文件系统物理路径。应用发现机制会自动跳过无效的清单；没有 `apps/` 目录的现有 Agent，其应用列表为空。

<a id="browser-sdk"></a>

## 浏览器 SDK

在应用入口 HTML 文件的 `<head>` 或 `<body>` 中通过 `<script src="holon.js"></script>` 加载运行时提供的脚本。加载后会在全局暴露 `window.Holon`。

<a id="context"></a>

### `context()`

返回一个包含 SDK 版本、所属 Agent、应用标识与会话状态的 Promise：

```json
{
  "sdk_version": "1",
  "agent_id": "main",
  "app_id": "ping-card",
  "session": { "authenticated": true }
}
```

成功获取上下文并不代表应用提交的输入会被当作操作者指令。

<a id="requestrequesttype-payload-requestid"></a>

### `request(requestType, payload?, requestId?)`

向应用的 `request` 端点发送带版本的结构化请求。响应包含 `ok`、`version`、`request_id`、`agent_id`、`app_id`、`status` 和 `message_id`。
`status: "accepted"` 仅表示该请求已成功进入运行时队列，不代表业务操作已完成，也不保证 Agent 一定会执行该请求。

请求会保留 Webhook 来源元数据，其源标识为 `app:<app-id>`。诸如 `ping` 之类的请求类型属于应用层业务数据，并非已注册的运行时底层系统命令。

<a id="eventsoptions"></a>

### `events(options?)`

返回应用专属运行时事件的异步迭代器（Async iterable）。选项参数包括 `signal`（`AbortSignal` 控制实例）与 `lastEventId`（用于断点续传的序列标识）。退出迭代循环或中止 signal 均会关闭事件流。

每个事件对象包含 `version`、`agent_id`、`app_id`，以及带有 `sequence`、`type`、`timestamp`、`message_id` 和可选 `request_id` 的 `event` 载荷。当前的事件流主要推送来自该 App 请求的 `message_enqueued` 与 `message_processing_started` 事件。它**不是业务处理完成的结果流**，也不暴露 Agent 的全部底层行为。

重放依赖一个有界的时间窗口。在网络断开重连后，应当直接核查工作状态，而不要单纯将未收到事件当作未曾发生任何操作的证据。

<a id="hosting-and-access-limits"></a>

## 托管与访问约束

- 单个静态资源体积限制为 8 MiB，且必须使用受支持的静态文件格式：HTML、JavaScript、CSS、JSON、文本、SVG、常见图片格式或 Web 字体。
- 规范化后的物理路径必须严格位于应用根目录以内。禁止目录穿越（`../`）或通过软链接越界访问。
- 应用复用运行时的身份认证并共享其源（Origin）。远程浏览器访问必须具备已认证的会话；严禁在前端代码中硬编码 Bearer Token。
- 基线内容安全策略（CSP）允许同源资源与同源网络连接、内联脚本/样式，以及 `data:` 协议的图片/字体。外部脚本引用与跨源网络请求默认均被阻断。
- 应用不会获得独立的浏览器 Origin，不具备沙箱 iframe 隔离，也不会与宿主的会话凭据隔离开来。请只托管完全受信任的前端源码。

关于完整路由与身份认证细节，请参阅 [HTTP 控制平面参考](http-control-plane.md)。在对外暴露网络服务之前，请仔细评估[安全与执行边界](../concepts/security-and-execution-boundaries.md)。
