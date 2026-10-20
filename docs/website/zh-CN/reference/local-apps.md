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

根目录下的 `manifest.json` 是可选的。仅在该目录项确实不存在时，运行时在内存中使用以下默认元数据：

| 字段 | 无清单时的默认值 |
|---|---|
| `id`、`name` | 应用目录名 |
| `version` | `"0.0.0"`（未声明版本的占位值，不是缓存指纹） |
| `entry` | `"index.html"` |
| `description` | `null` |

默认入口必须是应用根目录内可读取的普通文件。运行时不猜测其他 HTML、不递归查找入口、不创建目录，也不写入清单。请求不存在的 `manifest.json` 资源仍返回 `404`；发现响应提供默认元数据，不生成虚拟的可下载清单。

显式提供的 `manifest.json` 必须不超过 64 KiB，以下字段仍为必填：

| 字段 | 必填 | 契约约束 |
|---|---|---|
| `id` | 是 | 必须与应用目录名称以及请求的应用 ID 完全一致。 |
| `name` | 是 | 非空的展示名称。 |
| `version` | 是 | 非空版本字符串。 |
| `entry` | 是 | 安全且非空的文件相对路径，指向应用根目录内的 HTML 文件。 |
| `description` | 否 | 简短描述。 |

无效 JSON 或字段、不可读清单、目录冒充清单、悬空或越界软链接、超过 64 KiB 的清单均报错，即使同时存在 `index.html` 也不回退。根内软链接仍须指向符合大小限制的可读普通文件。自定义入口保持支持；显式入口文件缺失时仍在访问时返回 `404`。

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

- 普通发布文件不受扩展名白名单限制。`.wasm` 使用 `application/wasm`；`.data`、未知类型和无扩展名文件使用 `application/octet-stream`。托管脚本文件不代表在服务端执行它。
- 包括入口页在内，单文件默认上限为 64 MiB（`67108864` bytes）。Range 请求也必须满足完整文件大小限制；超限返回 `413` / `app_asset_too_large`，恰好等于上限允许。清单解析仍有独立的 64 KiB 上限。
- 规范化后的路径必须位于所选应用根目录内。禁止路径穿越、软链接逃逸及 `.env`、`.git` 等隐藏路径组件。不提供目录列表；应用目录只应包含准备发布的文件。
- 应用复用运行时的身份认证并共享其源（Origin）。远程浏览器访问必须具备已认证的会话；严禁在前端代码中硬编码 Bearer Token。
- 平台默认不附加 App `Content-Security-Policy` 响应头。操作者可以配置完整策略；应用自身的 meta CSP 和浏览器 CORS 仍然生效。配置 CSP 不会产生第三方应用沙箱。
- 应用不会获得独立的浏览器 Origin，不具备沙箱 iframe 隔离，也不会与宿主的会话凭据隔离开来。请只托管完全受信任的前端源码。

<a id="daemon-configuration"></a>

### Daemon 配置

`local_apps` 是 daemon 级配置对象，不是应用清单字段：

| 配置键 | 默认值 | 校验 |
|---|---|---|
| `local_apps.max_asset_bytes` | `67108864` | 正整数字节数；0 不代表无限。 |
| `local_apps.csp` | 未设置（`null`） | 完整的 HTTP header 值；非法值会被拒绝。 |

将上限改为 128 MiB，并按需设置同源策略：

```sh
holon config set local_apps.max_asset_bytes 134217728
holon config set local_apps.csp "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'"
```

该策略只是示例，不是默认值，也不保证适用于 Unity。使用 `holon config unset local_apps.max_asset_bytes` 或 `holon config unset local_apps.csp` 恢复对应默认值。

daemon 运行时，`config set` 和 `config unset` 通过现有配置 API 持久化修改并调度配置 reload。成功 reload 后，后续 App 请求使用新设置；在途流保留该请求已取得的策略。更新被拒绝时，CLI 不会静默回退到离线写入。直接编辑配置文件不会自动触发 reload；离线设置在 daemon 启动或重启后生效。应用不能覆盖这些设置；Web GUI 和 Explorer 的大小、CSP 与沙箱策略保持不变。

### 传输语义与容量

入口页与资源支持 `GET`、`HEAD`、单个 `Range`（`206`/`416`）、`ETag`、`304` 和 `If-Range`。`HEAD` 忽略 `Range`，无响应体且不读取文件内容。响应保留 `Cache-Control: no-store` 与 `X-Content-Type-Options: nosniff`；验证器不代表持久缓存承诺。

文件以 64 KiB 块流式发送，不整文件缓冲，读取不超过已检查的响应长度。文件增长不会扩大响应；截短或 I/O 错误会中断流。App 与 Explorer 共享传输代码，不共享授权或发布策略。

每个 HTTP `AppState` 在所有 Agent/App 间共享 16 个资源流 permit。全部占用时，新资源请求返回 `503` / `app_asset_busy`，并附带 `Retry-After: 1`。响应体完成、出错或被丢弃时释放 permit；无响应体时及时释放。SDK/API 端点不占用这些 permit。流缓冲预算不是进程 RSS 硬上限。

### Unity 与部署范围

首期目标是未预压缩、单线程 Unity Web 产物。`.gz`、`.br`、`.unityweb` 可以作为普通文件下载，但不代表预压缩构建可以运行，也不承诺正确的原始 MIME/`Content-Encoding` 协商。不新增多线程、COOP/COEP、SharedArrayBuffer 启用、反向代理或服务端执行能力。

真实游戏与真实 Mac/Safari 验收尚未完成，自动化 HTTP 检查不能代替这些结果。已有部署需要升级二进制并使配置生效；本文档变更本身不会部署或重启正在运行的服务。

关于完整路由与身份认证细节，请参阅 [HTTP 控制平面参考](http-control-plane.md)。在对外暴露网络服务之前，请仔细评估[安全与执行边界](../concepts/security-and-execution-boundaries.md)。
