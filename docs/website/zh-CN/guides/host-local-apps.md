---
title: 为 Agent 搭建与托管本地交互应用
summary: 在对话中直接让 Agent 在其 apps 目录下创建可视化任务看板，使用 window.Holon.request() 提交结构化请求并实时接收队列事件，无需手动查找目录或执行 CLI 命令。
order: 240
---

# 为 Agent 搭建与托管本地交互应用

Local App（本地应用）是由 Agent 自身托管的轻量级 Web 交互界面，静态文件存放在该 Agent 的 `apps/` 目录下。在处理任务审核队列、多条目巡检台账或结构化筛选时，纯文本聊天流往往翻找低效；而通过 Local App，你可以让 Agent 拥有专属于该工作流的可视化操作看板。

本指南演示如何在日常对话中直接指示 Agent 创建一个任务待审看板（Task Board），通过运行时内置的 App SDK（`holon.js`）打通双向交互、提交结构化操作请求，并在浏览器中实时监听队列与执行事件流。底层概念与安全边界请参阅[本地应用（Local Apps）概念](../concepts/local-apps.md)。

## 开始之前

- 本地已启动 Holon 并可访问 [Web GUI](./use-web-gui.md)（默认 `http://localhost:8080`），且已[配置可用模型](./configure-models.md)。
- 已有至少一个 Agent（如默认的 `main` 或从模板创建的专用 Agent）。
- 无需手动在宿主机磁盘翻找物理路径或执行底层 CLI 命令：直接在 Web GUI 对话中让 Agent 在其自身 `apps/` 目录下创建文件，即可自动存入正确的 Agent Home 空间中。

## 操作步骤

### 1. 在对话中指示 Agent 生成本地看板应用

在 Web GUI 聊天窗口中，向你的 Agent 发送提示词，要求其在自身的 `apps/` 目录下生成一个任务待审看板：

> "请在你的 `apps/task-board/` 目录下创建一个名为 `task-board` 的 Local App。包含 `manifest.json` 与 `index.html`。页面中引入 `holon.js`，通过 `window.Holon.context()` 展示你的 Agent ID，提供一个用于审批编号 #101 任务的按钮调用 `window.Holon.request()`，并使用 `window.Holon.events()` 实时滚动打印事件流日志。"

Agent 会在自身的存储区生成相应文件。

可选的清单文件 `manifest.json` 提供应用的元数据与展示信息：

```json
{
  "id": "task-board",
  "name": "任务审核看板",
  "version": "0.1.0",
  "entry": "index.html",
  "description": "由 Agent 自身托管的交互式审核小工具"
}
```

生成的 `index.html` 页面引入由运行时自动注入的 `holon.js` 客户端 SDK：

```html
<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Agent 任务看板</title>
  <style>
    body { font-family: system-ui, -apple-system, sans-serif; padding: 2rem; max-width: 600px; margin: auto; line-height: 1.5; }
    .card { border: 1px solid #e2e8f0; border-radius: 8px; padding: 1.5rem; margin-bottom: 1rem; background: #fff; }
    .badge { display: inline-block; padding: 0.2rem 0.6rem; border-radius: 4px; font-size: 0.85rem; font-weight: 600; background: #edf2f7; color: #4a5568; }
    button { background: #2563eb; color: #fff; border: none; padding: 0.6rem 1.2rem; border-radius: 6px; cursor: pointer; font-weight: 500; }
    button:disabled { background: #94a3b8; cursor: not-allowed; }
    pre { background: #0f172a; color: #e2e8f0; padding: 1rem; border-radius: 6px; overflow-x: auto; font-size: 0.85rem; }
  </style>
</head>
<body>
  <div class="card">
    <h2>Agent 任务看板</h2>
    <p id="context-info">正在连接 Agent 运行时...</p>
    <div style="margin: 1rem 0;">
      <span class="badge" id="agent-badge">Agent: 加载中...</span>
    </div>
    <button id="action-btn" disabled>审批通过待审项 #101</button>
  </div>

  <div class="card">
    <h3>最新请求入队回执</h3>
    <pre id="receipt-output">尚未提交请求。</pre>
  </div>

  <div class="card">
    <h3>实时 Agent 事件流</h3>
    <pre id="events-log">正在等待运行时事件...</pre>
  </div>

  <script src="holon.js"></script>
  <script>
    const contextEl = document.getElementById("context-info");
    const agentBadge = document.getElementById("agent-badge");
    const actionBtn = document.getElementById("action-btn");
    const receiptOutput = document.getElementById("receipt-output");
    const eventsLog = document.getElementById("events-log");

    async function init() {
      try {
        const ctx = await window.Holon.context();
        contextEl.textContent = `已连接到当前会话 (${ctx.surface || "web-gui"})。`;
        agentBadge.textContent = `所属 Agent: ${ctx.agent_id}`;
        actionBtn.disabled = false;
        observeEvents();
      } catch (err) {
        contextEl.textContent = `加载上下文失败: ${err.message}`;
      }
    }

    actionBtn.addEventListener("click", async () => {
      actionBtn.disabled = true;
      receiptOutput.textContent = "正在提交结构化请求至 Agent 队列...";
      try {
        const res = await window.Holon.request("review_item", {
          item_id: "item-101",
          decision: "approve",
          timestamp: Date.now()
        });
        receiptOutput.textContent = "入队回执（请求已受理进入队列）:\n" +
          JSON.stringify(res, null, 2);
      } catch (err) {
        receiptOutput.textContent = `错误: ${err.message}`;
      } finally {
        actionBtn.disabled = false;
      }
    });

    async function observeEvents() {
      try {
        for await (const item of window.Holon.events()) {
          const event = item.event;
          const time = new Date().toLocaleTimeString();
          eventsLog.textContent = `[${time}] ${event.type}: ${event.message_id || ""}\n` + eventsLog.textContent;
        }
      } catch (err) {
        eventsLog.textContent = `事件流已断开: ${err.message}\n` + eventsLog.textContent;
      }
    }

    init();
  </script>
</body>
</html>
```

### 2. 在浏览器中打开并体验双向交互

在浏览器中直接打开对应的逻辑路由 `/apps/<agent-id>/task-board/`。例如，若你的 Web GUI 运行在 `http://localhost:8080`，所属 Agent 为 `main`：

`http://localhost:8080/apps/main/task-board/`

*(切勿直接通过本地文件协议 `file://` 双击打开 HTML；App SDK 必须在运行时同源 HTTP 服务下工作)*。

1. **确认 Agent 上下文**：页面加载后，通过 `context()` 自动获取并显示该 App 归属的 Agent ID。
2. **点击操作按钮**：点击 **审批通过待审项 #101**。
3. **核验证明入队**：下方立刻打印入队回执，包含 `{ "status": "accepted", "message_id": "..." }`。
4. **核验实时事件**：在下方事件流中，实时捕获到 `message_enqueued` 以及后续由调度器派发的 `message_processing_started` 等事件。

### 3. 显式信任分类与安全边界

Local App 运行在严格的运行时信任边界之内：
- **入队回执不等于执行完毕**：`window.Holon.request()` 仅代表将请求放入 Agent 的工作收件箱；它返回的是已受理回执，而非最终执行完毕的结果。Agent 将根据自身的调度与业务规则处理该事项。
- **非操作者直接指令（Untrusted App Ingress）**：App 提交的请求在运行时被明确标记为 `origin: app_ingress` 与不受信来源。界面的按钮点击无法冒充人类操作者，不能越过 `AGENTS.md` 所规定的权限边界让 Agent 执行未获授权的破坏性操作。
- **同源会话保护**：Local App 与 Web GUI 共享运行时同源域名与已登录认证会话。切勿让 Agent 托管不可信的第三方前端代码。如需更严格的网络隔离，可通过配置文件定制 App Content Security Policy (CSP)。

## 何时制作 Local App，何时使用 Markdown 报告

- **选择 Markdown 报告**：当 Agent 的交付产物以阅读、归档或单次反馈为主时（例如每日站会总结、文档漂移审计清单、代码 Review 结论）。
- **选择 Local App**：当工作流需要频繁的人机交互、多条目筛选排序、可视化状态流转或重复的审核确认时（例如待审工单看板、发票报销分类小工具）。

## 托管已有静态构建

你也可以直接让 Agent 托管现成的静态文档、Web 资产或单线程 Unity Web 构建：
- 将构建产物放置在该 Agent 的 `apps/<app-id>/` 目录下。
- 确保根目录下包含入口 `index.html`。
- 静态资产（包括 `.wasm`、`.data`、图片、样式表等）均可直接对外提供，无服务端执行代码。
- 默认支持最大 64 MiB（即 `local_apps.max_asset_bytes` 配置项）的单个文件。

## 故障排查

- **应用未找到或 404**：检查目录名称是否与 URL 路由（`/apps/<agent-id>/<app-id>/`）完全一致。若存在 `manifest.json`，确认其 `id` 字段与目录名一致且入口文件存在。
- **鉴权失败（401）**：确保在已登录 Web GUI 的同一个浏览器中打开该应用，严禁在 HTML 页面中硬编码密钥。
- **SDK `holon.js` 404**：`holon.js` 由 Holon 运行时在 `/apps/{agent}/{app}/holon.js` 路由自动注入，确保你是通过 Holon 的 Web 服务访问，而非通过外部静态 Web 服务器访问。
- **`503 app_asset_busy`**：共享资源读取流短暂占满，浏览器会在接收到 `Retry-After: 1` 响应后自动重试。

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
- [本地应用概念](../concepts/local-apps.md) — 深入理解 Local App 架构与生命周期模型
- [本地应用参考规范](../reference/local-apps.md) — 查阅清单文件格式与 daemon 配置参数
- [观察 Agent 工作](observe-agent-work.md) — 查看 Agent 如何处理来自应用的排队任务
- [HTTP 控制平面参考](../reference/http-control-plane.md) — 路由规范与鉴权说明
