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

## 1. 创建目录与可选清单文件

在已确认的 Agent Home 中创建 `apps/ping-card/`。无需清单也能托管根目录下的 `index.html`：运行时用目录名作为 `id` 和 `name`，`version` 为 `"0.0.0"`，`entry` 为 `"index.html"`，`description` 为 `null`。默认值仅存在于内存中；Holon 不写入清单，也不修改应用文件。

若要使用本例的展示名称和描述，可选择保存 `apps/ping-card/manifest.json`：

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

- **应用未找到或被拒绝访问：** 无清单时须提供可读的根目录 `index.html`，运行时不会猜测其他入口。存在清单时，检查必填字段、`id`（本例为 `ping-card`）及入口路径。无效、超限、不可读、目录冒充、悬空链接或越界链接清单均报错，不回退到默认值。路径不得包含隐藏组件或逃出应用根目录。
- **鉴权失败：** 在登录或配对后，从运行时同源地址直接打开应用。严禁在 HTML 或 JavaScript 中硬编码 Bearer Token。
- **SDK 不可用：** 检查 `holon.js` 请求的响应。源码构建必须包含 App SDK 静态资源；单独的 HTML 页面无法自包含这些实现。
- **未收到任何事件：** 在浏览器控制台的 Network 面板中检查 `/events` 请求。刷新页面并在连接建立后重新提交请求。切勿将网络断开期间遗漏的事件误判为服务故障。
- **外部脚本或跨域请求被拦截：** 检查浏览器 CORS、应用自身的 CSP 及操作者配置的 App CSP。Holon 默认不附加平台 App CSP。不要在运行时的共享会话中引入不受信任的代码。
- **`413` / `app_asset_too_large`：** 完整文件超过 daemon 的 `local_apps.max_asset_bytes`（默认 64 MiB），即使只请求一个 Range 也会被拒绝。
- **`503` / `app_asset_busy`：** 16 个共享资源流均被占用。遵循 `Retry-After: 1` 后重试；SDK/API 请求不占这些 permit。

## 托管已有静态构建

对于静态站点或未预压缩、单线程的 Unity Web 构建，将准备发布的产物复制到已确认 Agent Home 的 `apps/<app-id>/`，保留相对目录结构和根目录 `index.html`，登录后访问 `/apps/<agent-id>/<app-id>/`。单纯托管静态文件不需要单独的 HTTP 服务、清单或 SDK 集成。如果构建使用其他 HTML 入口名，请提供完整的显式清单并设置 `entry`；Holon 不会猜测或重命名文件。

普通文件不受扩展名白名单限制，包括 WASM（`application/wasm`）、data 和未知类型（`application/octet-stream`）。隐藏文件及路径/软链接逃逸被拒绝；不提供目录列表或服务端执行。不要把源码目录、密钥或私有文件复制到应用目录。

daemon 级大小上限、可选 CSP、CLI 示例与生效时机详见 [Daemon 配置](../reference/local-apps.md#daemon-configuration)。daemon 运行时，`holon config set` 和 `holon config unset` 通过现有配置 API 持久化修改并调度 reload。成功 reload 后，后续 App 请求使用新设置；在途流保留已取得的策略。直接编辑文件不会自动触发配置 reload；离线设置在 daemon 启动或重启后生效。这些设置不改变 Web GUI 或 Explorer 的沙箱策略。

在浏览器 Network 面板检查入口、脚本、WASM 和 data 响应，然后实际操作应用，不要把下载成功当成游戏运行成功。`.gz`、`.br`、`.unityweb` 按普通文件提供，不承诺预压缩构建可运行或原始 MIME/编码协商正确。多线程 Unity 与 COOP/COEP 不在首期范围。真实游戏与真实 Mac/Safari 验收仍未完成，自动化 HTTP 检查不能证明这些结果。已有服务需要升级二进制并使配置生效；本文档不会重启它们。

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
