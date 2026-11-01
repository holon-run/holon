---
title: 开始使用
summary: 安装 Holon、配置模型，并验证你的第一个 Agent 交付结果。
order: 10
---

# 开始使用

Holon 以可安装的正式发布形式分发。本节给出从安装到运行 Agent 的最短路径，并展示如何根据后续需求选择合适的方向。

## 最短路径

```bash
brew tap holon-run/tap && brew install holon
holon onboard
holon daemon start
```

打开 <http://localhost:7878>，或运行 `holon tui`。第一次成功运行意味着你能够：

1. 向默认的 main agent 发送一项有明确边界的请求；
2. 看到 Agent 检查真实工作区或运行经批准的工具；
3. 收到精炼的交付结果，或收到明确需要下一步决策的提示。

下方教程提供完整分步说明，涵盖提供商配置、安装、连接 TUI 和创建第一个 Agent。

对于第一个持久工作流，建议选择一项天然包含等待环节的职责：跟进 PR 与 CI、等待证据到位的问题排查，或者需要明确人工审批的发布协调。

## 该用哪种运行模式？

Holon 提供三种与运行时交互的方式：

| 模式 | 命令 | 适合场景 |
|------|---------|----------|
| **单次执行** | `holon run "..."` | 在单次命令调用中完成的有界任务 |
| **Daemon + TUI** | `holon daemon start` + `holon tui` | 交互式 Agent 会话，带状态、队列和工作区 |
| **Daemon + HTTP** | `holon daemon start` + HTTP 客户端 | 外部集成、自动化脚本、控制平面消费端 |

下方的第一个 Agent 教程涵盖具名 Agent 的创建与客户端跟进，并给出了针对简单提问的单次执行备选方案。单次命令调用内部可以包含多轮模型与工具交互。

## 评估还是探索？

如果你已经熟悉 Holon，或想直接了解特定主题：

- **[运行长生命周期任务](/zh-CN/guides/run-long-lived-task)** — 跟踪进度、等待外部事件并验证完成状态
- **[委派工作给另一个 Agent](/zh-CN/guides/delegate-work)** — 把有界任务交给子 Agent
- **[与团队共用](/zh-CN/guides/share-with-your-team)** — 部署到共享服务器、接入团队登录，让全组协同交办任务给同一个 AI 助手
- **[通过 HTTP 自动化 Holon](/zh-CN/guides/automate-over-http)** — 用代码驱动 Holon
- **[概念](/zh-CN/concepts/)** — Agent、复用角色、工作区与持续工作的心智模型
- **[CLI 参考](/zh-CN/reference/cli.md)** — 完整命令行接口
- **[排查 Holon 任务问题](/zh-CN/guides/troubleshooting)** — 诊断常见的安装与运行问题

## 贡献还是内核开发？

如果你打算修改 Holon 本身或为其做贡献：

- **[本地运行 Holon](/zh-CN/maintainers/local-runtime)** — 保守的本地开发工作流程
- **[文档工作流](/zh-CN/maintainers/documentation-workflow)** — 如何构建和预览本站
- 仓库 `docs/` 目录 — RFC、实现决策和架构笔记

## 环境要求

- Holon 已安装并可在 `PATH` 中找到（Homebrew 或直接下载二进制；详见下方教程）
- 拥有受支持模型提供商的访问权限及所需凭据

## 社区交流与反馈

- 加入 [Holon Pioneers Google Group](https://groups.google.com/g/holon-pioneers) 交流使用心得、探讨工作流配方并参与新版本测试；
- 关注 X 官方账号 [@holonrun](https://x.com/holonrun) 获取产品发版与特性动态；
- 在 [GitHub Issues](https://github.com/holon-run/holon/issues) 反馈缺陷与提出功能建议。

## 寻找内核贡献者规格？

如果你正在参与 Holon 运行时内核开发或需要查看内部状态机契约，请参阅面向维护者的[运行时规格](/zh-CN/spec/)与仓库开发指南。

<!-- INDEX:START -->

- [创建你的第一个 Agent](./first-agent.md)
  安装 Holon、配置模型，并从具名 Agent 获得经核实的交付结果。
  <!-- mdorigin:index kind=article -->

- [引导配置](./onboarding.md)
  用 `holon onboard` 交互式完成提供商、凭据、模型和搜索配置。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
