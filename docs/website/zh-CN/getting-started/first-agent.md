---
title: 创建你的第一个 Agent
summary: "从零到你的第一个 Holon Agent：安装、启动、TUI 基础、创建 Agent、配置模型。"
order: 20
updated: 2026-05-23
---

# 创建你的第一个 Agent

本指南带你从零创建第一个 Holon Agent。你将：

1. 安装 Holon
2. 启动运行时（单次执行与 daemon 模式）
3. 用 TUI 连接
4. 创建 Agent
5. 配置模型

**耗时：** 约 10 分钟（Homebrew）；约 15 分钟（源码构建）

## 前置条件

- **Holon** 已安装并在 `PATH` 上（见第 1 步）
- 一个**模型提供商**账号（Anthropic、OpenAI 或兼容服务）
- 基本的**终端**操作能力

## 第 1 步：安装 Holon

Holon v0.14.0 及以后版本以可安装发布形式分发。选择适合你的方式。

### 方式 A：Homebrew（推荐）

```bash
brew tap holon-run/tap
brew install holon
```

### 方式 B：直接下载二进制

从 [GitHub Releases 页面](https://github.com/holon-run/holon/releases/latest)
下载对应平台的最新二进制，放到 `PATH` 上：

```bash
# 示例：Linux amd64
curl -L https://github.com/holon-run/holon/releases/latest/download/holon-linux-amd64 -o holon
chmod +x holon
sudo mv holon /usr/local/bin/
```

可用平台：Linux amd64、macOS amd64、macOS arm64。

### 方式 C：源码构建

如果你偏好源码构建或打算给 Holon 做贡献：

```bash
git clone https://github.com/holon-run/holon.git
cd holon
cargo build --release
```

源码构建时，把下文示例中的 `holon` 命令替换为 `cargo run --`。例如
`holon --help` 变成 `cargo run -- --help`。

### 验证安装

```bash
holon --help
```

你应该看到带可用命令列表的 CLI 帮助输出。

## 第 2 步：启动 Holon 运行时

Holon 有两种运行模式：

- **CLI 模式**（默认）：单次命令，直接输出
- **Daemon 模式**：后台服务，支持 TUI

### 方式 A：CLI 模式（快速上手）

执行单条命令：

```bash
holon run "What is Holon?"
```

执行一轮后退出。适合快速任务，不适合交互式会话。

### 方式 B：Daemon 模式（Agent 场景推荐）

启动后台 daemon：

```bash
holon daemon start
```

这会把 Holon 作为后台服务启动，提供：

- **Unix socket** 位于 `~/.holon/run/holon.sock`（本地访问）
- 供 TUI 和 HTTP 客户端使用的**控制平面**
- 跨会话的**持久状态**

#### 验证 daemon 正在运行

```bash
holon daemon status
```

你应该看到包含默认 Agent 的运行时状态。

#### 停止 daemon

```bash
holon daemon stop
```

## 第 3 步：用 TUI 连接

**终端 UI（TUI）** 提供与 Agent 协作的交互界面。

### 启动 TUI

```bash
holon tui
```

TUI 默认连接本地 Unix socket。

### TUI 基础

TUI 界面包含：

- **Agent 列表**：当前 Agent 及其状态
- **活跃 Agent**：正在接收你输入的 Agent
- **对话记录**：会话历史
- **任务列表**：后台任务和工作项

### 基本操作

- **输入**消息后回车发送
- **Ctrl+C** 退出 TUI
- **方向键**或 **Page Up/Down** 滚动历史

### 远程 TUI（可选）

连接远程 Holon 实例：

```bash
# 在远程主机上
holon serve --access lan --host 192.168.1.10 --token-file ~/.holon/remote.token

# 从本地机器
holon tui --connect http://192.168.1.10:7878 --token-file ~/.holon/remote.token
```

详见 [Remote TUI Access RFC](https://github.com/holon-run/holon/blob/main/docs/rfcs/remote-tui-access.md)（英文）。

## 第 4 步：创建 Agent

Holon 支持面向不同用途的多种 Agent 类型。

### 默认 Agent

启动 Holon 时，它会自动在 `~/.holon/agents/main/` 创建一个**默认 Agent**。该 Agent：

- 拥有自己的 `agent_home` 和 `AGENTS.md`
- 可以有 Agent 本地 skills
- 保存会话历史和工作状态

### 创建命名 Agent

为特定角色创建专门化 Agent：

```bash
holon agent create reviewer
```

这会在 `~/.holon/agents/reviewer/` 创建一个使用默认角色契约初始化的新 Agent。

### 使用模板

模板提供可复用的 Agent 配置：

```bash
# 用已安装或已同步的模板 ID
holon agent create docs-helper --template holon-developer

# 用本地模板路径
holon agent create custom --template /path/to/template

# 用 GitHub 模板 URL
holon agent create github-agent --template https://github.com/owner/repo/tree/main/template-path
```

### 列出 Agent

```bash
holon agent list
```

### 在 TUI 中切换 Agent

在 TUI 中，通过 Agent 列表视图切换 Agent。

## 第 5 步：配置模型

### 推荐：用 `holon onboard` 快速配置

最快的配置方式是交互式引导向导：

```bash
holon onboard
```

在终端中，这会启动交互式 TUI，带你完成：

1. **选择提供商** — 从内置提供商（Anthropic、OpenAI、DeepSeek、Codex 等）或自定义提供商中选择
2. **录入凭据** — OpenAI Codex 走浏览器 OAuth 登录；其他提供商输入 API key
   （输入不会回显，也不会记入日志）
3. **选择模型** — 选择默认模型，或输入自定义模型 ID
4. **搜索配置** — 启用 DuckDuckGo 托管搜索、模型原生搜索，或保持禁用

`holon onboard` 会写入配置、安全保存凭据并打印摘要。这是首次配置的推荐路径。

### 手动配置（备选）

Holon 需要模型配置才能与 Anthropic、OpenAI 等提供商协作。

### 配置分层

Holon 使用三层配置：

1. **启动设置**（环境变量、CLI 标志）
2. **运行时配置**（`config.json`）
3. **Agent 状态**（按 Agent 覆盖）

详见 [Runtime Configuration Surface RFC](https://github.com/holon-run/holon/blob/main/docs/rfcs/runtime-configuration-surface.md)（英文）。

### 设置提供商凭据

#### 方式 A：环境变量（快速上手）

```bash
# Anthropic
export ANTHROPIC_AUTH_TOKEN="your-api-key"

# OpenAI
export OPENAI_API_KEY="your-api-key"
```

#### 方式 B：凭据存储（持久配置推荐）

安全保存凭据，避免暴露在 shell 历史或环境变量中：

```bash
holon config credentials set --kind api_key --stdin anthropic
# 粘贴你的 ANTHROPIC_AUTH_TOKEN 并回车
```

### 设置默认模型

```bash
holon config set model.default "anthropic@default/claude-sonnet-4-6"
```

### Agent 级模型覆盖

单个 Agent 可以覆盖默认模型：

```bash
holon agent model set "anthropic@default/claude-sonnet-4-6" reviewer
```

Agent 模型覆盖的更多细节见[配置参考](/reference/configuration.md)（英文）。

### 验证模型配置

```bash
holon config get model.default
holon config doctor
```

查看可用模型：

```bash
holon config models list
```

## 下一步

你的第一个 Agent 已经跑起来了，接下来可以：

- **了解概念**：阅读[运行时模型](/concepts/runtime-model.md)（英文）和[信任边界](/concepts/trust-boundaries.md)（英文）
- **试试示例**：参见[快速示例](/guides/quick-examples.md)（英文）
- **构建集成**：查看[集成指南](/guides/integration.md)（英文）
- **参考文档**：[CLI 参考](/reference/cli.md)、[HTTP 控制平面](/reference/http-control-plane.md)、[配置参考](/reference/configuration.md)（英文）

## 故障排查

### daemon 启动失败

检查是否有其他实例在运行：

```bash
holon daemon status
holon daemon stop
holon daemon start
```

### TUI 连接不上

确认 daemon 正在运行且 socket 存在：

```bash
ls ~/.holon/run/holon.sock
holon daemon status
```

### 模型错误

检查凭据：

```bash
echo $ANTHROPIC_AUTH_TOKEN
holon config get model.default
```

更多帮助见[故障排查指南](/guides/troubleshooting.md)（英文）。
