---
title: 开始使用
summary: 从零开始，15 分钟内创建你的第一个 Holon Agent。
order: 10
---

# 开始使用

Holon 以可安装的发布形式分发。本节给出从安装到运行 Agent 的最短路径，
并告诉你接下来该往哪里走。

## 最短路径

```bash
brew tap holon-run/tap && brew install holon
holon onboard
holon daemon start
```

打开 <http://localhost:7878>，或运行 `holon tui`。第一次使用成功意味着你能够：

1. 向默认 main agent 发送一项有明确边界的请求；
2. 看到 Agent 检查真实工作区或运行已获允许的工具；
3. 收到精炼结果，或一项明确的下一步决策请求。

需要完整分步说明时，进入[创建你的第一个 Agent](first-agent.md)。

## 第一次接触 Holon？

如果你是第一次使用 Holon：

- **[引导配置](onboarding.md)** — 通过 `holon onboard` 交互式完成提供商、凭据、模型和搜索配置
- **[创建你的第一个 Agent](first-agent.md)** — 安装、启动、连接 TUI、创建 Agent、配置模型，约 15 分钟

教程涵盖：

- 安装 Holon 并启动运行时
- 用终端 UI（TUI）连接
- 创建 Agent 并发送第一条提示
- 配置模型和提供商

第一条持续工作流可以选择一项天然需要等待的职责：跟进 PR 与 CI、等待证据的问题分析，
或保留明确人工审批点的发布协调。

## 该用哪种运行模式？

Holon 提供三种与运行时交互的方式：

| 模式 | 命令 | 适合场景 |
|------|---------|----------|
| **单次执行** | `holon run "..."` | 快速的单轮任务，不需要 daemon |
| **Daemon + TUI** | `holon daemon start` + `holon tui` | 交互式 Agent 会话，带状态、队列和工作区 |
| **Daemon + HTTP** | `holon daemon start` + HTTP 客户端 | 集成、自动化、控制平面消费方 |

[第一个 Agent 教程](first-agent.md)使用 daemon + TUI，因为它提供完整的交互体验。
单次执行参见[快速示例](/guides/quick-examples)（英文）。

## 评估或探索？

如果你已经熟悉 Holon，或想直接深入细节：

- **[快速示例](/guides/quick-examples)**（英文）— 单次执行与常见任务模式
- **[持久 Agent 工作流](/guides/durable-agent-workflow)**（英文）— 持久 Agent 工作的完整生命周期
- **[概念](/zh-CN/concepts/)** — 深入内部机制前的心智模型
- **[CLI 参考](/reference/cli.md)**（英文）— 完整命令面
- **[故障排查](/guides/troubleshooting)**（英文）— 诊断常见安装问题

## 贡献或开发？

如果你打算修改 Holon 本身或为其做贡献：

- **[本地运行时指南](/guides/local-runtime)**（英文）— 保守的开发工作流
- **[文档工作流](/guides/documentation-workflow)**（英文）— 如何构建和预览本站
- **[集成指南](/guides/integration)**（英文）— 把 Holon 接入外部系统
- 仓库 `docs/` 目录 — RFC、实现决策和架构笔记

## 环境要求

- Holon 已安装并在 `PATH` 上（Homebrew 或直接下载二进制；分步说明见[创建第一个 Agent](first-agent.md)）
- 一个模型提供商 API key（Anthropic、OpenAI 或兼容服务）

## 仓库导览（贡献者）

这是给贡献者的简短导览。最终用户不需要了解仓库布局。

- `src/` 包含 Rust 运行时实现和可执行入口。
- `tests/` 包含 Rust 集成测试和共享测试支持。
- `docs/` 包含运行时契约、设计记录和当前架构笔记。
- `agent_templates/` 包含可远程同步的 Agent 模板。
- `docs/website/` 包含本 mdorigin 文档站。

## 本节页面

- [创建你的第一个 Agent](first-agent.md)
  从零到你的第一个 Holon Agent：安装、启动、TUI 基础、创建 Agent、配置模型。

- [引导配置](onboarding.md)
  用 `holon onboard` 交互式完成提供商、凭据、模型和搜索配置。

<!-- INDEX:START -->

- [创建你的第一个 Agent](./first-agent.md)
  从零到你的第一个 Holon Agent：安装、启动、TUI 基础、创建 Agent、配置模型。
  <!-- mdorigin:index kind=article -->

- [引导配置](./onboarding.md)
  用 `holon onboard` 交互式完成提供商、凭据、模型和搜索配置。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
