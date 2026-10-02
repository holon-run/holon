---
title: 托管 Local App
summary: 为 Agent 托管静态 HTML/JS 应用，通过内置 App SDK 与 Agent 交互，构建轻量级前端工具。
order: 25
---

# 托管 Local App

在 v0.47.0 中，Holon 引入了 Local App Engine。Agent 可以在其 `agent_home/apps/` 目录下托管静态 HTML 和 JavaScript 应用。

客户端通过逻辑 URL 访问这些应用，不需要直接暴露宿主机的文件路径。

## 运行机制

每个应用存放在对应 Agent 目录下的独立子目录中：

```text
~/.holon/agents/<agent_id>/apps/<app_id>/
├── manifest.json
├── index.html
├── style.css
└── app.js
```

当浏览器访问 `/apps/<agent_id>/<app_id>/` 时，Holon 服务端会：

1. 校验该目录及 `manifest.json` 的有效性。
2. 注入严格的内容安全策略（CSP）响应头并执行路径穿越检查。
3. 返回清单文件中指定的入口 HTML 文件及静态资源。
4. 在 `/apps/<agent_id>/<app_id>/holon.js` 提供预构建的浏览器端 SDK。

## 清单文件契约（manifest.json）

每个应用根目录下必须包含一个合法的 `manifest.json` 文件（大小不超过 64 KiB）：

```json
{
  "id": "status-monitor",
  "name": "状态监控卡片",
  "version": "1.0.0",
  "entry": "index.html",
  "description": "展示 Agent 状态并触发健康检查"
}
```

### 字段说明

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | 应用唯一标识。必须与所在目录名完全一致。 |
| `name` | string | 应用名称。 |
| `version` | string | 版本号（如 `1.0.0`）。 |
| `entry` | string | 应用入口 HTML 文件的相对路径。 |
| `description` | string | 可选的应用简要说明。 |

## 浏览器端 App SDK

Holon 在每个应用的路由根下内置提供了 `@holon/app-sdk` 脚本。在入口 HTML 中直接引入即可：

```html
<script src="holon.js"></script>
```

该脚本会在全局环境中注入 `window.Holon` 对象，提供以下三个核心方法：

### 1. `window.Holon.context()`

获取当前应用的运行时元数据及认证状态：

```javascript
const ctx = await window.Holon.context();
console.log(ctx.agent_id, ctx.app_id, ctx.session.authenticated);
```

返回格式示例：

```json
{
  "sdk_version": "1",
  "agent_id": "main",
  "app_id": "status-monitor",
  "session": { "authenticated": true }
}
```

### 2. `window.Holon.request(requestType, payload, requestId)`

向对应 Agent 消息队列发送结构化请求：

```javascript
const response = await window.Holon.request("run_diagnostics", { verbose: true });
console.log("请求已入队，ID:", response.request_id);
```

该方法向 `/apps/<agent_id>/<app_id>/request` 发起 `POST` 请求，并在事件日志中保留来源标记。

### 3. `window.Holon.events(onEvent, onError)`

通过 Server-Sent Events（SSE）订阅由应用发起的生命周期事件：

```javascript
const eventSource = window.Holon.events(
  (event) => console.log("收到事件:", event),
  (err) => console.error("事件流异常:", err)
);

// 完成后关闭连接：
// eventSource.close();
```

## 安全边界与限制

Local App Engine 包含以下明确的安全限制：

- **同源会话机制：** 托管应用与 Holon 守护进程共享同一源（Origin），自动携带已有的控制面会话 Cookie，但不会获得超出当前会话范围的额外特权。
- **内容安全策略（CSP）：** 服务端默认下发严格的 CSP 策略，仅允许加载同源（`'self'`）脚本、样式、图片与网络请求，阻止内联脚本执行和外部未知脚本注入。
- **静态资源白名单：** 单个资源文件体积限制在 8 MiB 以内。只允许托管常见静态文件格式（HTML、JS、CSS、JSON、TXT、SVG、主流图片与字体文件），未知扩展名或可执行文件会被服务端直接拦截。
- **路径沙箱防御：** 服务端会对每个静态资源请求进行路径规范化（Canonicalize），阻止包含 `../` 的路径穿越请求，并拒绝指向应用根目录以外的符号链接。

## 完整示例：构建一个 Ping 测试卡片

下面通过一个简单示例演示如何编写并运行一个 Local App。

### 1. 创建目录与清单

创建目录 `~/.holon/agents/main/apps/ping-card/`，并在其中创建 `manifest.json`：

```json
{
  "id": "ping-card",
  "name": "Ping Card",
  "version": "0.1.0",
  "entry": "index.html",
  "description": "通过轻量静态卡片与 Agent 互相 Ping"
}
```

### 2. 编写 `index.html`

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <title>Agent Ping 卡片</title>
  <style>
    body { font-family: sans-serif; padding: 2rem; max-width: 480px; margin: auto; }
    button { padding: 0.5rem 1rem; cursor: pointer; }
    pre { background: #f4f4f4; padding: 1rem; border-radius: 4px; overflow-x: auto; }
  </style>
</head>
<body>
  <h2>Agent Ping 卡片</h2>
  <p id="status">正在加载上下文...</p>
  <button id="ping-btn" disabled>发送 Ping</button>
  <pre id="output"></pre>

  <script src="holon.js"></script>
  <script>
    const statusEl = document.getElementById("status");
    const pingBtn = document.getElementById("ping-btn");
    const outputEl = document.getElementById("output");

    async function init() {
      try {
        const ctx = await window.Holon.context();
        statusEl.textContent = `已连接到 Agent: ${ctx.agent_id}`;
        pingBtn.disabled = false;
      } catch (err) {
        statusEl.textContent = `加载上下文失败: ${err.message}`;
      }
    }

    pingBtn.addEventListener("click", async () => {
      pingBtn.disabled = true;
      outputEl.textContent = "正在提交请求...";
      try {
        const res = await window.Holon.request("ping", { timestamp: Date.now() });
        outputEl.textContent = JSON.stringify(res, null, 2);
      } catch (err) {
        outputEl.textContent = `发生错误: ${err.message}`;
      } finally {
        pingBtn.disabled = false;
      }
    });

    init();
  </script>
</body>
</html>
```

### 3. 在浏览器中打开

启动 Holon 守护进程：

```bash
holon daemon start
```

在浏览器中访问 `http://127.0.0.1:7878/apps/main/ping-card/`。页面会自动调用 `window.Holon.context()` 显示连接的 Agent，点击按钮即可发送请求。

## 下一步

- [使用 Web GUI](/zh-CN/guides/use-web-gui.md) — 在浏览器中管理 Agent 与浏览工作区文件。
- [HTTP 控制平面参考](/zh-CN/reference/http-control-plane.md) — 了解 `/apps` 相关端点与认证机制。
- [通过 HTTP 自动化操作](/zh-CN/guides/automate-over-http.md) — 了解使用 HTTP API 驱动 Agent 的更多模式。
