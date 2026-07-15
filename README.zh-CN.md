# Holon

[English](README.md) | 中文

[![Release](https://img.shields.io/github/v/release/holon-run/holon?sort=semver)](https://github.com/holon-run/holon/releases/latest)[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Holon 是 **为 Agent 提供持续工作环境的本地工作台**。

Holon 本身不是 Agent，而是为多个 Agent 提供本地工作环境。Agent 负责理解目标并推进执行；Holon 把"工作"作为基本单位，负责保存状态、组织上下文、记录等待与唤醒，让跨会话、跨命令、跨人工确认或外部事件的任务能够在合适的时间恢复，并最终把结果交付给操作者。

## Holon 提供什么？

| 能力 | 说明 |
|---|---|
| **可延续的 Agent 工作现场** | 每个 Agent 在 Holon 中都有自己的连续工作现场，而不是随每次终端、请求或客户端连接重新开始。 |
| **Work-first 的任务模型** | Holon 把任务、等待、执行进展和最终交付组织为明确的 Work，而不是散落在一次次对话里。 |
| **事件驱动的等待与唤醒** | Agent 可以等待任务结果、外部事件或操作者输入，并在条件满足时回到对应工作继续推进。 |
| **明确的上下文与信任边界** | Holon 区分操作者输入、外部事件、工具结果和内部执行痕迹，避免不同来源的信息被混在一起。 |
| **本地优先的执行环境** | Holon 面向本地仓库、shell、worktree 和开发工具链，让 Agent 在真实工作环境中执行任务。 |

> Keep agent work alive in your local workspace.

## 快速开始

Holon 提供两种交互方式：**TUI**（终端）和 **Web GUI**（浏览器）。

### 1. 安装

```bash
brew tap holon-run/tap && brew install holon
```

或从 [GitHub Releases](https://github.com/holon-run/holon/releases/latest) 下载预编译二进制。

### 2. 配置 Provider

```bash
holon onboard
```

交互式引导配置 Provider 凭据。也可以在启动 daemon 后通过 Web GUI 的
**Settings** 页面配置 Provider。详见
[Configuration Reference](docs/website/reference/configuration.md) 和
[Web GUI 指南](docs/website/guides/web-gui.md)。

### 3. 启动 daemon

```bash
holon daemon start
```

### 4a. TUI（终端）

```bash
holon tui
```

选择 Agent 即可开始工作。TUI 断开后 Agent 仍会在 daemon 中继续运行。

### 4b. Web GUI（浏览器）

打开 <http://localhost:7878>，创建 Agent 并通过聊天界面工作，
内置文件浏览器、任务跟踪等功能。

更多：[TUI 指南](docs/website/guides/tui.md) · [Web GUI 指南](docs/website/guides/web-gui.md) · [首个 Agent](docs/website/getting-started/first-agent.md)

## 安装

```bash
brew tap holon-run/tap
brew install holon
holon --help
```

也可以从 [GitHub Releases](https://github.com/holon-run/holon/releases/latest)
下载 Linux amd64、macOS amd64 和 macOS arm64 预编译二进制。

下文示例假设 `holon` 已在 `PATH` 中。

## Provider 配置

Holon 需要配置模型提供商后才能运行 Agent。推荐方式：

- **`holon onboard`** — 交互式 CLI 引导，安全配置 Provider 凭据，不会回显密钥。
- **Web GUI Settings** — 启动 daemon 后打开 <http://localhost:7878>，
  通过 Settings 页面配置 Provider。

Holon 支持 Anthropic、OpenAI、DeepSeek、OpenRouter、Qwen、GLM、Xiaomi、Kimi、
MiniMax 等常见 Provider。高级配置（credential profile、自定义 Provider、
Codex 订阅等）请看
[Configuration Reference](docs/website/reference/configuration.md) 和
[Supported Models](docs/website/reference/models.md)。

## 核心概念

Holon 把 Agent 工作拆成几个明确的运行时对象：

- **Agent** — 长期存在的本地身份，拥有自己的队列、状态和工作现场。
- **WorkItem** — 可持续推进的目标，包含计划、进度、阻塞、等待条件和完成报告。
- **Task** — 可监督的异步执行（命令、后台任务或子 Agent）。
- **WaitFor / wake** — 显式声明等待任务结果、外部事件或操作者输入，条件满足时恢复。
- **Workspace / worktree** — 在本地仓库中执行，把编码任务隔离到托管 worktree。
- **Origin / brief** — 保留输入来源和信任信息，把执行痕迹与操作者可见交付分开。

更详细的概念说明请看 [Concepts](docs/website/concepts/)。

## 状态与兼容性

Holon 正在积极开发中。当前推荐版本为
[`v0.29.0`](https://github.com/holon-run/holon/releases/tag/v0.29.0)。

当前项目重心仍是 Rust 运行时：agent 生命周期、队列、WaitFor/wake、task、WorkItem、
信任边界、本地 workspace 和结构化交付。

## 文档

- [Website docs](https://holon.run) — 安装、入门、概念、指南和参考
- [Documentation layers](docs/website/concepts/documentation-layers.md)
- [Architecture overview](docs/architecture-overview.md)
- [RFCs](docs/rfcs/README.md)
- [Implementation decisions](docs/implementation-decisions/README.md)
- [Release process](docs/release.md)

## 从源码构建

Rust 二进制在编译时通过 `rust-embed` 嵌入 Web GUI 资源。先构建前端再编译二进制：

```bash
make all
holon --help
```

或分步：

```bash
make web    # 构建 Web GUI（需要 Node.js）
make build  # 构建 Rust 二进制
```

运行检查：

```bash
make ci
```

完整目标列表见 `make help`。

运行基准测试：

```bash
cd benchmark
npm install
npm test
```

## 社区

- [GitHub Discussions](https://github.com/holon-run/holon/discussions)
- [GitHub Issues](https://github.com/holon-run/holon/issues)

## 许可证

本项目采用 [Apache-2.0](LICENSE) 许可证。
