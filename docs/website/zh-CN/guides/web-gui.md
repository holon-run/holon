---
title: Web GUI
summary: 使用 Holon 内置的 Web 界面，在浏览器中管理 Agent、监控运行时状态并配置设置。
order: 25
---

# Web GUI

Holon 内置了一个由 daemon 直接提供的 Web GUI。无需单独构建或部署前端——启动 daemon，打开浏览器即可。

## 快速开始

```bash
# 启动 daemon（Web GUI 默认启用）
holon daemon start
```

然后在浏览器中打开 [http://127.0.0.1:7878/](http://127.0.0.1:7878/)。

Web GUI 由编译进 `holon` 二进制的内嵌资源提供。默认 CORS 配置允许 `localhost` 来源，因此在本机上开箱即用。

> **注意：** 如果你配置了自定义监听地址或端口，请相应调整 URL。

## 页面

### 仪表盘

仪表盘提供运行时总览：

- **Agent 名册** — 所有 Agent 及其状态（Awake、Asleep、Booting）、待处理消息数和当前模型。
- **运行时健康** — 调度器状态、唤醒提示和近期活动。
- **任务列表** — 点击任务打开详情面板，查看状态、命令、workdir 和输出。任务事件（创建、状态更新、完成）会实时刷新列表。
- **快捷操作** — 创建 Agent、挂载 workspace、查看 Agent 详情。
- **Agent 生命周期** — 直接在仪表盘上启动、停止和删除 Agent。删除会永久移除该 Agent 及其数据，可选择一并移除它的私有子 Agent。

### Agent 会话

从仪表盘选择 Agent 即可打开它的会话页面：

- **消息流** — 以线索式会话展示近期消息、工具调用和简报。
- **显示级别** — 在 Info（紧凑的面向用户输出）、Verbose（工具调用和中间结果）与 Debug（完整运行时元数据，含工具执行记录）之间切换。
- **输入栏** — 向选中的 Agent 发送 operator 消息。
- **事件时间线** — 侧栏展示近期事件的时间线，包括轮次、工具执行和状态转换。
- **虚拟滚动** — 消息列表采用虚拟化渲染，长对话下依然流畅，且不带来 DOM 开销。
- **工具执行详情** — 在消息流中展开单次工具调用，查看请求/响应载荷、耗时和元数据。工具结果面板同时展示结构化输出和原始数据，便于调试 Agent 行为。

### 搜索

在浏览器中搜索 Agent 记忆。

结果包含：

- **摘录** — 每条结果展示高亮匹配词的上下文片段，无需打开完整记录即可判断相关性。
- **可展开的来源** — 点击结果就地展开完整内容，无需离开搜索页。
- **按 Agent 过滤** — 把结果限定在一个或多个 Agent ID 上。
- **全文搜索** — 跨 Agent 查询运行时记忆索引。

### Agent 模板

可以直接在 Web GUI 中浏览、安装模板并据此创建 Agent。页面位于 `/templates`：

- **模板目录** — 浏览已安装模板的显示名、描述和来源信息（本地、远程 URL 或已同步来源）。
- **创建 Agent** — 点击模板打开已预填模板选择器的“创建 Agent”对话框。Agent 会按模板的角色契约和预装 skills 初始化。
- **远程来源** — 查看和管理已配置的远程模板来源（GitHub 仓库）。daemon 启动时会从这些来源同步模板。
- **模板详情** — 点击模板查看完整元数据，包括模板 manifest、预装 skills 和来源。

命令行管理模板见 [Agent 模板指南](/zh-CN/guides/agent-templates.md)。

### Skill 管理

在浏览器中管理 Skill Library 和 Agent skills：

- **库目录** — 浏览本地 Skill Library 中注册的全部 skills，含名称、描述和来源信息。
- **添加 skill** — 从本地路径、远程 URL 或 GitHub `uses` 简写导入 skill。
- **移除 skill** — 从库中移除 skill。
- **启用/停用** — 用开关按 Agent 启用或停用单个 skill，并可查看每个 Agent 当前生效的 skills。
- **Skill 详情** — 点击 skill 查看完整元数据，包括 scope、source root 和发现路径。

Skill 管理页面位于 Web GUI 的 `/skills`。daemon 运行内嵌 GUI 时，也可以从导航侧栏进入。

通过 Web GUI 安装 skill 是**非阻塞**的：在浏览器中添加 skill 后，安装会作为后台任务运行（见 [Job API](#job-monitoring)）。进度指示器显示当前状态，任务状态存在 localStorage 中，刷新页面也能继续跟踪。

Skills 页面还支持**更新**已安装的 skill。远程来源中发布了新版本的 skill 会显示更新按钮，点击后会创建一个拉取最新版本的目录更新任务。任务完成后就地显示成功或错误反馈，更新任务同样以后台任务运行（见 [Job API](#job-monitoring)）。Skill 目录更新也作为任务运行。

命令行管理 skill 见 [Skills 指南](/zh-CN/guides/skills.md)。

### Workspace 文件浏览器

挂载 workspace 后，可以从左侧栏打开 workspace 文件浏览器：

- **目录树** — 用可折叠的树视图浏览 workspace 目录，子目录可展开/折叠。
- **文件预览** — 点击文件在主面板预览内容。文本文件带语法高亮内联渲染；二进制和图片文件显示元数据和下载链接。
- **图片渲染** — 图片文件在预览面板中，以及通过 `workspace://` URI 在会话消息中内联渲染。支持 PNG、JPEG、GIF 和 WebP。
- **可调整面板** — 拖动文件树和内容面板之间的分隔条调整布局。
- **拖放附件** — 把文件从文件浏览器或桌面拖入 Agent 输入栏，作为 operator 消息附加。图片和文本文件内联附加；其他类型以元数据引用形式出现。拖到会话区域上方时会显示放置提示。
- **独立文件查看页** — 通过文件树右键菜单在全页查看器中打开文件，获得更大的阅读区域。

文件浏览器使用 workspace 文件浏览 API（`GET /api/workspaces/{id}/files` 和 `GET /api/workspaces/{id}/files/{path}`）。路径穿越和 symlink 逃逸会被拦截；单次文件读取上限为 1 MB。

### 导航改进

Web GUI 还包含若干导航和易用性改进：

- **导航栈** — 页面维护历史栈，返回按钮会回到上一视图并保留滚动位置和状态，而不是重置回仪表盘。
- **文件级刷新** — 文件浏览器支持按文件刷新，无需重载整个页面。文件工具栏中的刷新按钮会重新拉取选中文件的内容。
- **工具栏** — 文件查看页带有工具栏，包含刷新和 markdown 源码/渲染切换等操作。
- **自动滚动** — 有新内容到达时（例如流式日志输出），文件查看器自动滚动到底部。
- **Markdown 渲染视图** — 查看 `.md` 文件时，可在渲染后的 HTML 和原始 markdown 源码之间切换。渲染视图支持语法高亮的代码块、表格和链接。

### 设置

在浏览器中配置 Holon：

- **模型设置** — 查看和修改默认模型、覆盖单个 Agent 的模型，并设置推理强度。备用模型列表使用 chip 组件，支持拖动排序，便于调整优先级。
- **API key** — 通过凭据存储添加或更新提供商凭据（API key），无需手动改 JSON 文件。设置页会为每个提供商自动判断合适的凭据方式：api_key 提供商走 API key 输入，Codex 这类 OAuth 提供商走设备登录链接。
- **Ollama 发现** — 本机运行的 Ollama 服务会被自动发现，无需 API key；其模型会出现在模型选择器中。
- **语言** — 切换 Web GUI 显示语言，支持英文（EN）和简体中文（ZH-CN）。所有界面文案（导航、按钮、标签、状态消息）都会实时更新，无需重载页面。
- **图像生成** — 直接在设置页配置默认图像生成模型（`image_generation.default`）。模型选择器只显示支持图像生成能力的模型。
- **运行时配置** — 查看当前执行环境、已挂载的 workspaces 和策略快照。

### 国际化（i18n）

Web GUI 使用 react-i18next 做完整国际化。设置页的语言选择器可在英文和简体中文之间切换。所有界面文案（导航项、按钮标签、表单提示、状态消息和错误页）都会在选中后立即切换。翻译资源与 UI 资源一起编译进内嵌构建，无需外部文件或网络访问。

设置页还包含 **search provider** 区块，可在浏览器中配置网页搜索和原生搜索提供商。

### UI 图标

Web GUI 的状态指示、导航图标和文件浏览器图标现在统一使用 lucide 图标（通过 lucide-react），取代了之前的 Unicode 符号方案。工具提示经过精简以更清晰，面板标题统一使用 `label(N)` 格式。

### 检查器（右侧面板）

查看 Agent 或仪表盘时，右侧面板显示：

- **Agent 身份** — Agent ID、可见性、归属和 profile preset。
- **当前工作** — 进行中的工作项、计划状态和 todo 清单。
- **Token 用量** — 累计及每轮 token 消耗。
- **活跃子 Agent** — 派生的子 Agent 及其状态。
- **工具延迟** — 每个工具的调用次数和总耗时。

右侧面板也承载上下文详情视图。例如，在仪表盘中点击任务会在主视图旁打开**任务详情**面板，显示状态、类型、命令、workdir 和输出。

右侧面板支持展开为全屏模式；模型菜单通过固定定位 portal 渲染，不会被布局溢出裁剪。

## 远程访问

Web GUI 兼容 Holon 的远程访问模式。daemon 配置为远程访问（tunnel、tailnet 或 LAN）后，通过同一端点打开 GUI URL：

```bash
# 示例：从同一网络中的另一台机器经 LAN 访问
http://<daemon-host>:7878/
```

从不同来源访问时需要配置 CORS。详见[远程访问](/zh-CN/guides/remote-access)和[配置](/zh-CN/reference/configuration)。

## 内嵌构建与开发构建

| 模式 | 访问方式 | 适用场景 |
|------|--------------|-------------|
| **内嵌**（默认） | `holon daemon start` → `/` | 日常使用 |
| **开发服务器** | `cd web-gui/app && npm run dev` | UI 开发 |

内嵌构建在发布时通过 `rust-embed` 编译进 `holon` 二进制。生产使用无需单独执行 `npm` 安装或构建。

做 UI 开发时启动开发服务器：

```bash
cd web-gui/app
npm install
npm run dev
```

开发服务器支持热重载，没有 Holon 服务端在运行时使用 fixture 数据。可设置 `HOLON_API_PROXY_TARGET` 把开发服务器的 `/api` 代理指向运行中的 Holon daemon（默认 `http://127.0.0.1:7878`）。

## 性能诊断

Web GUI 通过 `/api/control/runtime/performance` 暴露运行时性能指标。该端点返回按阶段分组的细粒度计时数据：

| 分组 | 指标 |
|-------|---------|
| `turn.*` | 总轮次时间、上下文构建、provider 轮次、工具执行、清理 |
| `provider.*` | 请求构建、轮次总计、重试延迟 |
| `tool.execution` | 工具执行累计计时 |
| `storage.*` | 事件追加和状态持久化计时 |
| `projection.*` | Agent 状态投影子步骤（任务、定时器、工作项等） |
| `http.*` | 按路由的 HTTP 响应计时 |
| `scheduler.*` | 按结果的轮询延迟 |

每个指标包含 `count`、`total_ms`、`max_ms` 和 `avg_ms`。可用来诊断慢轮次、找出耗时的工具，或长期跟踪 provider 延迟。

### Job 监控

skill 安装这类长时间操作会作为可跟踪的任务运行，而不是阻塞请求：

- **就地进度** — Skills 页面为运行中的任务显示进度指示器，完成时给出成功或错误反馈。
- **Job API** — 运行时通过 `/api/jobs/{job_id}` 暴露任务读模型，包含状态、阶段和进度项。

## 另请参阅

- [快速示例](/zh-CN/guides/quick-examples) — 用几条命令试用 Holon
- [远程访问](/zh-CN/guides/remote-access) — 连接远程 daemon
- [故障排查](/zh-CN/guides/troubleshooting) — 诊断常见问题
- [配置参考](/zh-CN/reference/configuration) — CORS、端口和控制平面设置
