---
title: 使用 Web GUI
summary: 在浏览器里驱动 Agent、Work Item 和 Skill。
order: 16
---

# 使用 Web GUI

Web GUI 就装在 daemon 里：启动 daemon，打开浏览器，就可以不碰终端地操作 Agent、
工作项、Skill 和文件。本页讲第一次使用和几个主要界面。

## 快速开始

```bash
# 启动 daemon（Web GUI 默认启用）
holon daemon start
```

然后在浏览器中打开 [http://127.0.0.1:7878/](http://127.0.0.1:7878/)。

Web GUI 由编译进 `holon` 二进制的内嵌资源提供。默认 CORS 配置允许 `localhost` 来源，因此在本机上开箱即用。

> **注意：** 如果你配置了自定义监听地址或端口，请相应调整 URL。

## 身份认证与登录

在远程访问或启用了认证的环境下，浏览器访问控制台需要先在 `/login` 完成认证：

- **本地 Token 模式** (`auth.mode = "local"`)：输入静态 Control Token，系统将自动换取 HttpOnly 的 `holon_session` Cookie。
- **OIDC 单点登录模式** (`auth.mode = "oidc"`)：点击 **Continue with organization login**，跳转至企业 IdP 完成认证。

有关 IdP 接入与会话超时策略的完整配置，请参阅[配置 OIDC 身份认证](/zh-CN/guides/configure-oidc-authentication.md)。

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
- **实时 SSE 流式传输** — 直连 `/api/agents/:id/events/stream`，实现思考过程（reasoning tokens）、活跃工具执行状态指示器与即时简报（briefs）的流式刷新。
- **调度等待与交付状态指示** — 当 Agent 执行 `WaitFor`（等待后台命令任务、外部回调或用户输入）时，界面显式呈现等待原因与交付模式（`final` 或 `silent`）。

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

命令行管理模板见 [Agent 模板指南](/zh-CN/reference/agent-templates.md)。

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

命令行管理 skill 见 [Skills 指南](/zh-CN/reference/skills.md)。

### Workspace 文件浏览器

从右侧面板的 **文件** 标签进入，或点击会话中的文件引用。选中文件后直接显示正文；Markdown 支持“预览 / 源码”切换，文本文件支持语法高亮。

- **浏览目录**：切回所在目录，保留选中文件和阅读位置；展开面板后可以并排查看目录与正文。
- **位置栏**：切换工作区、浏览上层目录。长路径折叠中间目录；点击“文件信息”查看完整路径、执行根、类型、大小和修改时间。
- **文件操作**：复制路径、Markdown 引用、网页链接，以及下载和刷新，都在 `⋯` 菜单中。新窗口按钮可以打开独立阅读页面。
- **返回来源**：面板顶部的返回按钮沿文件浏览历史返回原来的详情；右上角关闭按钮关闭整个面板。

文件定位保留工作区和执行根身份。路径穿越与符号链接越界会被拦截；文本预览上限为 1 MB，下载可获取完整文件。

#### 可选的 Finder 集成（macOS）

当 Holon 直接运行在浏览器所在的 Mac 上时，可以在启动时明确开启：

```bash
holon daemon start --access local --desktop-integration
# 对已经运行的本机实例，有意重启后启用：
holon daemon restart --access local --desktop-integration
```

Holon macOS 菜单应用启动或重启它管理的 daemon 时，会自动添加 `--desktop-integration`。文件操作菜单会出现 **在 Finder 中显示**，用于定位文件，不会打开或执行文件内容。其他平台仍支持预览、复制和下载。

直接使用 CLI 启动时，此选项默认关闭，需要手动指定；菜单应用每次启动或重启都会开启。CLI 重启会继承设置，也可以用 `--desktop-integration=false` 覆盖。运行时只允许回环地址监听。不要对端口转发、反向代理或容器实例启用：`localhost` 并不能证明文件属于当前电脑。因此界面显示“本机地址”，不据此断言“本机运行”。

### 设置

在浏览器中配置 Holon：

- **模型设置** — 查看和修改默认模型、覆盖单个 Agent 的模型，并设置推理强度。备用模型列表使用 chip 组件，支持拖动排序，便于调整优先级。
- **API key** — 通过凭据存储添加或更新提供商凭据（API key），无需手动改 JSON 文件。设置页会为每个提供商自动判断合适的凭据方式：api_key 提供商走 API key 输入，Codex 这类 OAuth 提供商走设备登录链接。
- **Ollama 发现** — 本机运行的 Ollama 服务会被自动发现，无需 API key；其模型会出现在模型选择器中。
- **语言** — 切换 Web GUI 显示语言，支持英文（EN）和简体中文（ZH-CN）。所有界面文案（导航、按钮、标签、状态消息）都会实时更新，无需重载页面。
- **图像生成** — 直接在设置页配置默认图像生成模型（`image_generation.default`）。模型选择器只显示支持图像生成能力的模型。
- **控制面访问与 Bearer Token** — 在远程访问或开启鉴权的环境中，支持在界面中直接配置并安全存储 Bearer 访问令牌。
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

从不同来源访问时需要配置 CORS。详见[远程访问](/zh-CN/guides/connect-remote-runtime)和[配置](/zh-CN/reference/configuration)。

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

- [运行你的第一个 Holon 任务](/zh-CN/guides/quick-examples) — 用几条命令试用 Holon
- [连接远程 Holon 运行时](/zh-CN/guides/connect-remote-runtime) — 从另一台机器使用 GUI
- [排查 Holon 任务问题](/zh-CN/guides/troubleshooting) — 诊断常见问题
- [配置参考](/zh-CN/reference/configuration) — CORS、端口和控制平面设置
- [配置 OIDC 身份认证](/zh-CN/guides/configure-oidc-authentication.md) — 设置团队 SSO 与会话策略
