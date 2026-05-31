# Holon

[English](README.md) | 中文

[![Release](https://img.shields.io/github/v/release/holon-run/holon?sort=semver)](https://github.com/holon-run/holon/releases/latest)[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Holon 是 **为 Agent 提供持续工作环境的本地工作台**。

Holon 本身不是 Agent，而是为多个 Agent 提供本地工作环境。Agent 负责理解目标并推进执行；Holon 把“工作”作为基本单位，负责保存状态、组织上下文、记录等待与唤醒，让跨会话、跨命令、跨人工确认或外部事件的任务能够在合适的时间恢复，并最终把结果交付给操作者。

## 目录

- [Holon 提供什么？](#holon-提供什么)
- [安装](#安装)
- [Provider 配置](#provider-配置)
- [快速开始](#快速开始)
- [核心概念](#核心概念)
- [状态与兼容性](#状态与兼容性)
- [项目边界](#项目边界)
- [文档](#文档)
- [从源码构建](#从源码构建)

## Holon 提供什么？

| 能力 | 说明 |
|---|---|
| **可延续的 Agent 工作现场** | 每个 Agent 在 Holon 中都有自己的连续工作现场，而不是随每次终端、请求或客户端连接重新开始。 |
| **Work-first 的任务模型** | Holon 把任务、等待、执行进展和最终交付组织为明确的 Work，而不是散落在一次次对话里。 |
| **事件驱动的等待与唤醒** | Agent 可以等待任务结果、外部事件或操作者输入，并在条件满足时回到对应工作继续推进。 |
| **明确的上下文与信任边界** | Holon 区分操作者输入、外部事件、工具结果和内部执行痕迹，避免不同来源的信息被混在一起。 |
| **本地优先的执行环境** | Holon 面向本地仓库、shell、worktree 和开发工具链，让 Agent 在真实工作环境中执行任务。 |

> Keep agent work alive in your local workspace.

## 安装

通过 Homebrew 安装最新版本：

```bash
brew tap holon-run/tap
brew install holon
holon --help
```

也可以从 [GitHub Releases](https://github.com/holon-run/holon/releases/latest)
下载 Linux amd64、macOS amd64 和 macOS arm64 预编译二进制。

下文示例假设 `holon` 已在 `PATH` 中。

## Provider 配置

Holon 需要配置模型提供商后才能运行 Agent。主要支持三类方式：

- **本地凭据存储**：推荐日常使用，通过 credential profile 管理 API key，
  避免依赖 daemon 启动前已经注入的环境变量。
- **内置 provider**：支持 Anthropic、OpenAI、DeepSeek、OpenRouter、Qwen、
  GLM、Xiaomi、Kimi、MiniMax 等常见 provider。
- **外部登录 / 自定义 provider**：`openai-codex/...` 可复用本地 `codex login`
  会话，支持 Codex 订阅；也可以接入兼容协议的自定义 provider。

推荐的本地配置方式是先保存 API key，再把 provider 指向对应的 credential profile：

```bash
printf '%s' "$DEEPSEEK_API_KEY" \
  | holon config credentials set --kind api_key --stdin deepseek

holon config providers set deepseek \
  --credential-source credential_profile \
  --credential-kind api_key \
  --credential-profile deepseek

holon config set model.default "deepseek/deepseek-v4-pro"

# 或使用本地 Codex 登录会话 / Codex 订阅
holon config set model.default "openai-codex/gpt-5.5"
```

检查配置状态：

```bash
holon config doctor
holon config models list
```

更多 provider、credential profile、自定义 provider 和模型目录，请看：

- [Configuration Reference](docs/website/reference/configuration.md)
- [Supported Models](docs/website/reference/models.md)

## 快速开始

### 1. 启动 daemon

先启动本地常驻运行时：

```bash
holon daemon start
holon daemon status
```

### 2. 连接 TUI

连接 TUI：

```bash
holon tui
```

### 3. 选择或创建 Agent

Holon 会自动提供默认的 main Agent。创建新 Agent 有两种方式：

- 在 TUI 中告诉 main Agent，让它为你创建。
- 也可以通过 CLI 创建：

```bash
holon agent create builder --template holon-developer
holon agent list
```

之后在 TUI 中选择 Agent 开始工作即可。TUI 断开后，Agent 仍会在 daemon
中继续运行。

更多操作请看 [TUI 命令参考](docs/website/reference/cli.md#terminal-ui) 和 [Daemon 管理](docs/website/reference/cli.md#daemon-management)。

## 核心概念

Holon 把 Agent 工作拆成几个明确的运行时对象：

- **Agent** 是长期存在的本地身份，拥有自己的队列、状态、历史和工作现场。
- **WorkItem** 表达一个可持续推进的目标，包含计划、进度、阻塞、等待条件和完成报告。
- **Task** 表达可监督的异步执行，例如命令、后台任务或子 Agent。
- **WaitFor / wake** 让 Agent 显式声明正在等待任务结果、外部事件或操作者输入，并在条件满足时恢复。
- **Workspace / worktree** 让 Agent 在本地仓库中执行，并可把编码任务隔离到托管 worktree。
- **Origin / brief** 保留输入来源和信任信息，并把内部执行痕迹与操作者可见交付分开。

这些概念共同解决的是同一个问题：让 Agent 的工作不依赖某一次聊天或终端连接，而是可以被观察、恢复、等待、委托和交付。

更详细的概念说明请看 [Concepts](docs/website/concepts/)。

## 当前版本

当前推荐版本为
[`v0.15.1`](https://github.com/holon-run/holon/releases/tag/v0.15.1)。

`v0.15.0` 是 Holon Rust runtime 进入公开兼容性维护阶段的基线版本。
从该版本开始，项目会开始维护 CLI、daemon/API 语义和本地持久化存储的兼容性。

完整变更请查看
[v0.15.1 Release Notes](https://github.com/holon-run/holon/releases/tag/v0.15.1)。

## 状态与兼容性

Holon 正在积极开发中。从 `v0.15.0` 开始，项目会把以下内容作为需要维护兼容性的公开契约：

- **CLI**：常用命令、参数和结构化输出需要保持可迁移；破坏性调整应通过发布说明和迁移路径说明。
- **接口**：daemon 客户端 API、事件语义和运行时对象字段需要保持向后兼容或提供明确的版本化演进方式。
- **持久化存储**：agent 状态、账本、消息、transcript、WorkItem 和 task 等本地数据需要支持升级与读取兼容。

当前项目重心仍是 Rust 运行时：agent 生命周期、队列、WaitFor/wake、task、WorkItem、信任边界、
本地 workspace 和结构化交付。

## 项目边界

Holon 聚焦于运行时语义：Agent 身份、工作延续性、执行状态、本地工作区投影和操作者可见结果。

相邻的 Holon Run 项目覆盖其他层：

- **[AgentInbox](https://github.com/holon-run/agentinbox)** — source hosting、activation 和 delivery
- **[UXC](https://github.com/holon-run/uxc)** — unified capability 和 tool access
- **[WebMCP Bridge](https://github.com/holon-run/webmcp-bridge)** — browser 与 web-app edge access

组合使用时，AgentInbox 负责把外部事件送达和唤醒 Holon；Holon 负责决定这些事件在运行时中的含义。

## 文档

Holon 的文档分为三个层次，详见
[documentation layers](docs/website/concepts/documentation-layers.md)。

**使用 Holon：**

- [Website docs](https://holon.run) — 安装、入门、概念、指南和当前参考
- [Security and execution boundaries](docs/website/concepts/security-and-execution-boundaries.md)

**集成与运维：**

- [Local operator troubleshooting](docs/local-operator-troubleshooting.md)
- [Release process](docs/release.md)

**贡献运行时：**

- [Architecture overview](docs/architecture-overview.md) — 从这里开始
- [RFCs](docs/rfcs/README.md) — 规范设计契约
- [Implementation decisions](docs/implementation-decisions/README.md) — 设计理由

## 社区

- [GitHub Discussions](https://github.com/holon-run/holon/discussions)
- [GitHub Issues](https://github.com/holon-run/holon/issues)

## 从源码构建

```bash
cargo install --path .
holon --help
```

## 开发

运行检查：

```bash
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo check --all-targets
cargo test --all-targets -- --test-threads=1
```

运行基准测试：

```bash
cd benchmark
npm install
npm test
```

## 许可证

本项目采用 [Apache-2.0](LICENSE) 许可证。
