---
title: 托管本地应用
summary: 构建一个由 Agent 拥有的轻量本地应用，提交请求并在浏览器中验证队列事件。
order: 240
---

# 托管本地应用

构建一个可展示所属 Agent 信息、提交 `ping` 请求并实时呈现事件流的交互卡片。此流程用于验证 App SDK 的连通性，同时不会向该应用开放执行任意任务的特权。底层心智模型请参阅[本地应用（Local Apps）](../concepts/local-apps.md)。

## 开始之前

- 使用已有 Agent 并确认其 Agent Home 路径。可以让该 Agent 在其主目录下创建应用，或使用已确认的主机路径；自定义数据目录可能会改变实际存储位置。请避免覆盖同名的 `ping-card` 应用。
- 执行 `holon daemon status` 检查运行时状态。若未启动，请执行 `holon daemon start`。
- 使用运行时的浏览器同源地址；远程访问时需具备有效会话。请参阅[连接到远程运行时](connect-remote-runtime.md)。
- 仅托管你信任的代码。本地应用与运行时共享同源域和会话，并不对主机凭据构成隔离屏障。

## 1. 创建目录与清单文件

在已确认的 Agent Home 中创建 `apps/ping-card/` 目录，并保存 `manifest.json`：

```json
{
  "id": "ping-card",
  "name": "Ping Card",
  "version": "0.1.0",
  "entry": "index.html",
  "description": "Ping the agent from a static card"
}
```

## 2. 编写页面代码

创建 `apps/ping-card/index.html`：

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <title>Agent Ping Card</title>
  <style>
    body { font-family: sans-serif; padding: 2rem; max-width: 480px; margin: auto; }
    button { padding: 0.5rem 1rem; cursor: pointer; }
    pre { background: #f4f4f4; padding: 1rem; border-radius: 4px; overflow-x: auto; }
  </style>
</head>
<body>
  <h2>Agent Ping Card</h2>
  <p id="status">正在加载上下文...</p>
  <button id="ping-btn" disabled>Ping Agent</button>
  <pre id="output"></pre>
  <pre id="events"></pre>

  <script src="holon.js"></script>
  <script>
    const statusEl = document.getElementById("status");
    const pingBtn = document.getElementById("ping-btn");
    const outputEl = document.getElementById("output");

    async function init() {
      try {
        const ctx = await window.Holon.context();
        statusEl.textContent = `已连接至 Agent: ${ctx.agent_id}`;
        pingBtn.disabled = false;
        observeEvents();
      } catch (err) {
        statusEl.textContent = `加载上下文失败: ${err.message}`;
      }
    }

    pingBtn.addEventListener("click", async () => {
      pingBtn.disabled = true;
      outputEl.textContent = "正在发送请求...";
      try {
        const res = await window.Holon.request("ping", { timestamp: Date.now() });
        outputEl.textContent = "入队回执（非最终执行结果）:
" +
          JSON.stringify(res, null, 2);
      } catch (err) {
        outputEl.textContent = `错误: ${err.message}`;
      } finally {
        pingBtn.disabled = false;
      }
    });

    async function observeEvents() {
      const eventsEl = document.getElementById("events");
      try {
        for await (const item of window.Holon.events()) {
          const event = item.event;
          eventsEl.textContent += `${event.type}: ${event.message_id}
`;
        }
      } catch (err) {
        eventsEl.textContent += `事件流中断: ${err.message}`;
      }
    }

    init();
  </script>
</body>
</html>
```

## 3. 打开并测试应用

在浏览器的运行时同源地址下打开 `/apps/<agent-id>/ping-card/`，将 `<agent-id>` 替换为实际的 Agent ID。例如，在本地默认端口运行且 Agent ID 为 `main` 时，访问路径为 `http://127.0.0.1:7878/apps/main/ping-card/`。切勿直接通过 `file:` 本地文件协议打开 HTML。

1. 检查页面中显示的所属 Agent 名称是否正确，该信息来自 `context()`。
2. 等待事件流连接建立，点击一次 **Ping Agent**。
3. 检查返回的入队回执中是否包含 `status: "accepted"` 以及对应的 `message_id`。
4. 在下方事件流中匹配该 ID 的 `message_enqueued` 事件。若 Agent 处于可运行状态，还会进一步触发 `message_processing_started`。

事件流反映的是消息入队与处理事件，并非最终的业务完成结果。`ping` 请求属于结构化的消息输入，并非内置的可执行命令。请参阅[观察 Agent 工作](observe-agent-work.md)确认 Agent 实际执行的动作。应用请求的任何业务动作都必须单独获得操作者的指令授权。

## 故障排查

- **应用未找到或被拒绝访问：** 检查 `manifest.json` 格式是否合法、`id` 是否严格等于 `ping-card`，且入口文件是否存在于该目录下。
- **鉴权失败：** 在登录或配对后，从运行时同源地址直接打开应用。严禁在 HTML 或 JavaScript 中硬编码 Bearer Token。
- **SDK 不可用：** 检查 `holon.js` 请求的响应。源码构建必须包含 App SDK 静态资源；单独的 HTML 页面无法自包含这些实现。
- **未收到任何事件：** 在浏览器控制台的 Network 面板中检查 `/events` 请求。刷新页面并在连接建立后重新提交请求。切勿将网络断开期间遗漏的事件误判为服务故障。
- **外部脚本或跨域请求被拦截：** 仅使用同源静态资源。严禁为了引入不受信任的代码而放宽 CSP 内容安全策略。

## 契约细节

<a id="how-local-apps-work"></a>
关于所有权与请求流转心智模型，请参阅[本地应用（Local Apps）](../concepts/local-apps.md)。

<a id="manifest-contract"></a>
<a id="manifest-fields"></a>
[清单规范契约](../reference/local-apps.md#manifest-contract)定义了必填字段与校验规则。

<a id="the-browser-app-sdk"></a>
<a id="1-windowholoncontext"></a>
<a id="2-windowholonrequestrequesttype-payload-requestid"></a>
<a id="3-windowholoneventsoptions"></a>
[浏览器 SDK 参考](../reference/local-apps.md#browser-sdk)详细记录了返回值定义与事件流限额。

<a id="security-invariants"></a>
关于静态资源限制、鉴权机制与 CSP 规则，请参阅[托管与访问限制](../reference/local-apps.md#hosting-and-access-limits)。

<a id="example-building-a-minimal-app"></a>
<a id="1-create-the-directory-and-manifest"></a>
<a id="2-write-indexhtml"></a>
<a id="3-open-in-browser"></a>
上文给出的示例已取代早期版本的极简应用演示。

## 另见

- [使用 Web GUI](use-web-gui.md) — 管理 Agent 与查阅文件
- [HTTP 控制平面参考](../reference/http-control-plane.md) — 路由规范与鉴权说明
