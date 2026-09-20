---
title: 指南
summary: 面向使用、运维和集成 Holon 的任务指南。
order: 30
---

# 指南

按你要完成的工作选择指南：试用 Holon、使用 Web GUI、自动化 GitHub 任务、
运维运行时，或协调多个 Agent。

## 结构与内容边界

指南（Guides）是任务导向的实操文档（How-to），解答*“我想完成具体任务 X，应该怎么做？”*。每篇指南遵循一致的结构：
1. **目标与场景：** 达成什么目标、适用何种场景。
2. **前置条件：** 所需环境、凭证或权限。
3. **分步操作：** 最小可复现的命令与简要说明。
4. **验证与排错：** 如何确认执行成功，以及常见故障的排除。

指南仅保留完成任务所需的最小示例。全量 CLI 选项、配置键与 HTTP 接口详表请查阅 [参考](/zh-CN/reference/)；底层设计心智模型请查阅 [概念](/zh-CN/concepts/)。

下方列表说明了每篇指南的用途：

<!-- INDEX:START -->

- [持久 Agent 工作流](./durable-agent-workflow.md)
  端到端的持久 Agent 故事：创建 Agent、启动长任务、熬过断连、等待事件，并交付最终简报。
  <!-- mdorigin:index kind=article -->

- [holon solve](./solve.md)
  用 holon solve 在无头模式下自动处理 GitHub issue 和 pull request。
  <!-- mdorigin:index kind=article -->

- [本地运行时](./local-runtime.md)
  在本地运行和检查 Holon 的一套保守流程。
  <!-- mdorigin:index kind=article -->

- [TUI 指南](./tui.md)
  Holon 的交互式终端 UI —— 导航、斜杠命令、事件日志、模型选择与远程连接。
  <!-- mdorigin:index kind=article -->

- [快速示例](./quick-examples.md)
  完成入门指南后可以尝试的常见 Holon 任务。
  <!-- mdorigin:index kind=article -->

- [Workspace](./workspaces.md)
  Workspace 生命周期——附加、退出、分离、worktree 隔离，以及 workspace 与 shell 目录的区别。
  <!-- mdorigin:index kind=article -->

- [Agent 模板](./agent-templates.md)
  Agent 模板是什么、如何同步或安装、如何使用 --template，以及如何创建自定义模板。
  <!-- mdorigin:index kind=article -->

- [文档工作流](./documentation-workflow.md)
  如何编辑和构建由 mdorigin 驱动的 Holon 网站。
  <!-- mdorigin:index kind=article -->

- [远程访问](./remote-access.md)
  远程 daemon 访问——tunnel、tailnet、LAN 模式、token 管理，以及从远程 TUI 连接。
  <!-- mdorigin:index kind=article -->

- [Web GUI](./web-gui.md)
  使用 Holon 内置的 Web 界面，在浏览器中管理 Agent、监控运行时状态并配置设置。
  <!-- mdorigin:index kind=article -->

- [集成指南](./integration.md)
  通过 HTTP 控制平面以编程方式将外部系统集成到 Holon 的分步操作指南。
  <!-- mdorigin:index kind=article -->

- [故障排查](./troubleshooting.md)
  常见 Holon 问题的解决方案，涵盖 daemon、配置、模型和 TUI 问题。
  <!-- mdorigin:index kind=article -->

- [运行时可观测性](./observability.md)
  用 OTLP 导出 Holon trace，抓取受保护的 OpenMetrics，并安装基线仪表盘与告警。
  <!-- mdorigin:index kind=article -->

- [多 Agent 协作](./multi-agent.md)
  创建和调用 Agent、监督契约，以及用于并行工作的 workspace 模式。
  <!-- mdorigin:index kind=article -->

- [Skills 指南](./skills.md)
  可复用的 SKILL.md 工作流、skill 位置，以及如何开发自定义 skill。
  <!-- mdorigin:index kind=article -->

- [WebFetch 和 WebSearch 指南](./webfetch-websearch.md)
  用于抓取网页和搜索网络的 Agent 工具：工具参考、提取模式、搜索提供商和使用模式。
  <!-- mdorigin:index kind=article -->

- [工作项指南](./work-items.md)
  用工作项、计划、todo 清单和生命周期管理来跟踪持久目标。
  <!-- mdorigin:index kind=article -->

- [ViewImage 指南](./view-image.md)
  通过视觉模型检查本地图片的 Agent 工具：模型选择、视觉观察、持久元数据和缓存。
  <!-- mdorigin:index kind=article -->

- [图像生成指南](./image-generation.md)
  从文本提示词生成图片的 Agent 工具：模型选择、尺寸和格式选项、输出管理。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
