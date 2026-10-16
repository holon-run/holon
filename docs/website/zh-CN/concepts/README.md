---
title: 概念
summary: 理解 Agent、复用角色、工作区、持久化工作以及可依赖的系统边界。
order: 20
---

# 概念

这些页面阐述 Holon 核心对象的含义、它们之间的协作方式，以及你可以明确依赖的系统行为。建议先阅读运行时模型，再根据工作需要查阅对应的概念页面。

第一组页面涵盖 Agent 及其运行环境：指引体系、模板、Skill 与工作区。随后的内容跟踪持续推进的工作：目标、任务、事件、协作、记忆与上下文。其余页面解释模型路由、Local App、交付结果以及信任与访问边界。

如果你想快速跑通第一个请求，请参阅[入门指南](../getting-started/)；如果需要完成特定目标的具体步骤，请参阅[指南](../guides/)。精确的参数与 Schema 归属于[参考](../reference/)；内部状态机与维护者细节则归属于[运行时规格](../spec/)。

<!-- INDEX:START -->

- [运行时模型](./runtime-model.md)
  守护进程、客户端、Agent、工作项、任务与工作区如何协同运作。
  <!-- mdorigin:index kind=article -->

- [Agent、角色与指引](./agents.md)
  Agent 身份、Agent Home 以及定义 Agent 职责的指引机制。
  <!-- mdorigin:index kind=article -->

- [Agent 模板](./agent-templates.md)
  用于定义 Agent 初始角色与 Skill 的可复用起点，与由此创建的 Agent 实例彼此独立。
  <!-- mdorigin:index kind=article -->

- [Skill 与工具](./skills-and-tools.md)
  可复用工作流与工具能力、Skill 管理以及已加载 Agent 指引之间的关系。
  <!-- mdorigin:index kind=article -->

- [工作区与工作目录](./workspaces.md)
  Agent Home、项目工作区、执行根以及 worktree，以及工作区隔离的边界。
  <!-- mdorigin:index kind=article -->

- [工作项与任务](./work-items-and-tasks.md)
  持久化目标与受控执行句柄，包括规划、等待、权属与完成判定。
  <!-- mdorigin:index kind=article -->

- [事件、等待与唤醒](./events-and-waits.md)
  消息、任务结果、定时器与外部事件如何让 Agent 暂停并恢复已授权的工作。
  <!-- mdorigin:index kind=article -->

- [多 Agent 协作](./multi-agent-collaboration.md)
  对等 Agent、受监督的子 Agent，以及委托成果验收与清理权属。
  <!-- mdorigin:index kind=article -->

- [记忆系统](./memory.md)
  运行时证据、精选记忆与日常笔记，及其加载、搜索与权威边界。
  <!-- mdorigin:index kind=article -->

- [上下文连续性](./context-continuity.md)
  持久化工作状态如何帮助 Agent 在无需重放所有历史记录的情况下恢复执行，以及哪些中断仍需恢复。
  <!-- mdorigin:index kind=article -->

- [模型、提供商与路由](./models-and-providers.md)
  模型能力、可执行提供商路由、Agent 级覆盖、回退链、Fast 模式以及咨询决策。
  <!-- mdorigin:index kind=article -->

- [Local App](./local-apps.md)
  由 Agent 托管的交互界面，其请求与事件流转机制，以及共享会话访问的边界。
  <!-- mdorigin:index kind=article -->

- [交付结果、产物与可观察状态](./results-and-artifacts.md)
  如何区分已受理工作、执行进度、完成报告、任务输出与落盘文件。
  <!-- mdorigin:index kind=article -->

- [信任边界](./trust-boundaries.md)
  操作者输入、指引文件与外部证据在来源、指令权威和调度优先级上的本质区别。
  <!-- mdorigin:index kind=article -->

- [安全与执行边界](./security-and-execution-boundaries.md)
  宿主本地执行、工作区边界、认证访问以及能力凭证风险。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
