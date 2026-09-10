---
title: Holon
summary: 为持续工作的 Agent 提供的本地工作台。
order: 1
---

# Holon

**Holon 是一个为持续工作的 Agent 提供的本地工作台。**

Holon 本身不是 Agent。它为多个 Agent 提供本地工作环境：Agent 理解目标并驱动执行，Holon
把"工作"作为核心单元——保存状态、组织上下文、记录等待与唤醒，让跨会话、跨命令、跨人工确认或外部事件的任务都能在合适的时机恢复，并最终把结果交付给操作者。

## Holon 提供什么

| 能力 | 含义 |
|---|---|
| **持续的 Agent 工作区** | 每个 Agent 在 Holon 中拥有持续的工作上下文，而不是随终端、请求或客户端连接重启。 |
| **以工作为核心的任务模型** | Holon 把任务、等待、执行进度和最终交付组织成显式的工作单元，而不是散落在对话里。 |
| **事件驱动的等待与唤醒** | Agent 可以等待任务结果、外部事件或操作者输入，条件满足时自动回到对应的工作。 |
| **显式的上下文与信任边界** | Holon 区分操作者输入、外部事件、工具结果和内部执行痕迹，不同来源的信息不会被混在一起。 |
| **本地优先的执行环境** | Holon 面向本地仓库、shell、worktree 和开发工具链构建，让 Agent 在真实的工作环境中执行任务。 |

> 让 Agent 的工作在你的本地工作区里持续存活。

## 试用 Holon

Holon 提供两种交互方式：**TUI**（终端）和 **Web GUI**（浏览器）。

### 安装

```bash
brew tap holon-run/tap && brew install holon
holon --help
```

或从 [GitHub Releases](https://github.com/holon-run/holon/releases/latest) 下载二进制文件。

macOS 13 及以上用户可以从同一发布页安装通用 `Holon-<version>.dmg`，获得支持开机自启的原生菜单栏应用。

### 配置模型提供商

```bash
holon onboard
```

该命令会启动交互式引导，完成提供商凭据配置。启动 daemon 后，也可以通过 Web GUI 的
**Settings** 页面配置。详见[配置参考](/reference/configuration)（英文）和
[Web GUI 指南](/guides/web-gui)（英文）。

### 启动 daemon

```bash
holon daemon start
```

### TUI（终端）

```bash
holon tui
```

选择一个 Agent 开始工作。断开连接后 Agent 仍在继续运行。

### Web GUI（浏览器）

打开 <http://localhost:7878>。创建 Agent，通过聊天界面工作，内置文件浏览器、任务跟踪等能力。

Holon 会自动提供一个默认的 main agent。你可以通过 TUI 或 Web GUI 创建更多专门化的
Agent。更多信息：[开始使用](/zh-CN/getting-started/) ·
[TUI 指南](/guides/tui)（英文）· [Web GUI 指南](/guides/web-gui)（英文）

Holon 支持 Anthropic、OpenAI、DeepSeek、OpenRouter、Qwen、GLM、Xiaomi、Kimi、MiniMax
等提供商。高级配置见[配置参考](/reference/configuration)（英文）和
[支持的模型](/reference/models)（英文）。

## 核心概念

Holon 把 Agent 的工作拆解为几个显式的运行时对象：

- **Agent**：长存的本地身份，拥有自己的队列、状态、历史和工作上下文。
- **WorkItem（工作项）**：可持续推进的目标，包含计划、进度、阻塞项、等待条件和完成报告。
- **Task（任务）**：受监督的异步执行，例如命令、后台任务或子 Agent。
- **WaitFor / wake（等待与唤醒）**：让 Agent 显式声明正在等待任务结果、外部事件或操作者输入，并在条件满足时恢复。
- **Workspace / worktree（工作区）**：让 Agent 在本地仓库中执行，并通过受管理的 worktree 隔离编码任务。
- **Origin / brief（来源与简报）**：保留输入来源与信任信息，同时把内部执行痕迹与操作者可见的交付分开。

这些概念共同解决一个问题：Agent 的工作不应依赖于某一次聊天或终端连接。它应该是可观察、可恢复、可等待、可委托、可交付的。

## 状态与兼容性

当前推荐版本是
[`v0.39.0`](https://github.com/holon-run/holon/releases/tag/v0.39.0)。

`v0.15.0` 是 Holon Rust 运行时进入公开兼容性维护的基线版本。从该版本起，项目对 CLI、
daemon/API 语义和本地持久化存储维持兼容性预期。

Holon 仍在积极开发中。当前重心仍是 Rust 运行时：Agent 生命周期、队列、WaitFor/唤醒、
任务、WorkItem、信任边界、本地工作区，以及结构化交付。

## 项目边界

Holon 专注运行时语义：Agent 身份、工作连续性、执行状态、本地工作区投影，以及操作者可见的结果。

Holon Run 的相邻项目覆盖其他层面：

- **[AgentInbox](https://github.com/holon-run/agentinbox)** — 源托管、激活与交付
- **[UXC](https://github.com/holon-run/uxc)** — 统一的能力与工具访问
- **[WebMCP Bridge](https://github.com/holon-run/webmcp-bridge)** — 浏览器与 Web 应用边缘访问

组合使用时，AgentInbox 把外部事件送达并唤醒 Holon；Holon 在运行时内部决定这些事件的含义。

## 我该读哪些文档？

- **我想安装并运行 Holon** → [开始使用](/zh-CN/getting-started/)
- **我想理解这些概念** → [概念](/zh-CN/concepts/)，尤其是
  [运行时模型](/concepts/runtime-model)（英文）和
  [安全与执行边界](/concepts/security-and-execution-boundaries)（英文）
- **我想查命令或配置项** → [参考](/zh-CN/reference/)
- **我想集成 Holon** → [集成指南](/guides/integration)（英文）
- **我想为运行时做贡献** →
  [架构概览](https://github.com/holon-run/holon/blob/main/docs/architecture-overview.md)
  和 [RFCs](https://github.com/holon-run/holon/tree/main/docs/rfcs)（英文）

## 文档目录

- [运行时规格](./spec/)
- [开始使用](./getting-started/)
- [概念](./concepts/)
- [指南](./guides/)
- [参考](./reference/)

> 中文文档正在逐步完善中。尚未翻译的页面会链接到对应的英文版本。

<!-- INDEX:START -->

- [运行时规格](./spec/)
  <!-- mdorigin:index kind=directory -->

- [开始使用](./getting-started/)
  <!-- mdorigin:index kind=directory -->

- [概念](./concepts/)
  <!-- mdorigin:index kind=directory -->

- [指南](./guides/)
  <!-- mdorigin:index kind=directory -->

- [参考](./reference/)
  <!-- mdorigin:index kind=directory -->

<!-- INDEX:END -->
