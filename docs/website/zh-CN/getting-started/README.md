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

下方教程提供完整的分步说明，涵盖提供商配置、安装、连接 TUI 和创建第一个 Agent。

第一条持续工作流可以选择一项天然需要等待的职责：跟进 PR 与 CI、等待证据的问题分析，
或保留明确人工审批点的发布协调。

## 该用哪种运行模式？

Holon 提供三种与运行时交互的方式：

| 模式 | 命令 | 适合场景 |
|------|---------|----------|
| **单次执行** | `holon run "..."` | 快速的单轮任务，不需要 daemon |
| **Daemon + TUI** | `holon daemon start` + `holon tui` | 交互式 Agent 会话，带状态、队列和工作区 |
| **Daemon + HTTP** | `holon daemon start` + HTTP 客户端 | 集成、自动化、控制平面消费方 |

第一个 Agent 教程使用 daemon + TUI，因为它提供完整的交互体验。
单次执行参见[快速示例](/zh-CN/guides/quick-examples)。

## 评估或探索？

如果你已经熟悉 Holon，或想直接深入细节：

- **[快速示例](/zh-CN/guides/quick-examples)** — 单次执行与常见任务模式
- **[持久 Agent 工作流](/zh-CN/guides/durable-agent-workflow)** — 持久 Agent 工作的完整生命周期
- **[概念](/zh-CN/concepts/)** — 深入内部机制前的心智模型
- **[CLI 参考](/zh-CN/reference/cli.md)** — 完整命令面
- **[故障排查](/zh-CN/guides/troubleshooting)** — 诊断常见安装问题

## 贡献或开发？

如果你打算修改 Holon 本身或为其做贡献：

- **[本地运行时指南](/zh-CN/guides/local-runtime)** — 保守的开发工作流
- **[文档工作流](/zh-CN/guides/documentation-workflow)** — 如何构建和预览本站
- **[集成指南](/zh-CN/guides/integration)** — 把 Holon 接入外部系统
- 仓库 `docs/` 目录 — RFC、实现决策和架构笔记

## 环境要求

- Holon 已安装并在 `PATH` 上（Homebrew 或直接下载二进制；分步说明见下方教程）
- 一个模型提供商 API key（Anthropic、OpenAI 或兼容服务）

## 寻找内核贡献者规格？

如果你正在参与 Holon 运行时内核开发或需要查看内部状态机契约，请参阅面向维护者的 [运行时规格](/zh-CN/spec/) 与仓库开发指南。

<!-- INDEX:START -->

- [创建你的第一个 Agent](./first-agent.md)
  从零到你的第一个 Holon Agent：安装、启动、TUI 基础、创建 Agent、配置模型。
  <!-- mdorigin:index kind=article -->

- [引导配置](./onboarding.md)
  用 `holon onboard` 交互式完成提供商、凭据、模型和搜索配置。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
