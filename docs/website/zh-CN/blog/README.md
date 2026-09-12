---
title: 博客
summary: Holon 的产品思考、实践记录、操作指南与项目动态。
order: 4
---

# 博客

Holon 仍处于早期阶段。这里用于解释运行时背后的问题，记录真实实践中观察到的现象，并提供
可以在你自己的环境中复现的路径。

## 从这里开始

以下四篇为首批中文审阅稿，尚未发布；英文页面仅留待翻译占位。案例的证据与公开范围仍需确认。

- **产品介绍** — [Holon 是什么：关掉窗口以后，那项修复怎么办](./what-is-holon)
- **技术设计** — [Holon 的 WorkItem 架构：把工作状态从对话中分离](./why-work-items)
- **审阅案例** — [不止审一次代码：用 Holon 搭建持续跟进 PR 的 Reviewer](./one-pr-one-work-item)
- **团队案例** — [从个人 AI 工具到团队协作：一个小团队的 Agent Native 实践](./agents-in-a-small-team)

## 入门与早期草稿

下面保留原有文章与链接，不作为这四篇新稿的已审定版本。

- **思考** — [为什么 Agent 的工作不应止于一次聊天](./why-agent-work-should-outlive-the-chat)
- **实践** — [从 Issue 到发布：工作不会停在某个人的终端里](./project-work-that-survives-ci-waits)
- **指南** — [用 Holon 构建你的第一个长期 Agent 工作流](./first-long-lived-agent-workflow)

## 这里会发布什么

- **思考**：解释为什么长期、事件驱动的 Agent 工作需要运行时。
- **实践**：记录已观察到的工作流、证据范围和限制条件。
- **指南**：把运行时概念转化为可复现的步骤。
- **动态**：用于发布版本和重要项目变化。

产品契约和命令细节仍以文档为准。Blog 提供背景与实践解释；如果两者存在差异，应以当前
运行时文档和发布说明为准。

<!-- INDEX:START -->

- [Holon 是什么：让多个 Agent 在你的工作环境里持续做事](./what-is-holon.md)
  把反复发生的工作交给固定角色，在后台跟进，需要时接入。通过持续审阅的例子，认识 Holon 这套本地工作台及其在远程开发和团队协作中的用法。中文审阅稿，未发布。
  <!-- mdorigin:index kind=article -->

- [为什么 Agent 的工作不应止于一次聊天](./why-agent-work-should-outlive-the-chat.md)
  为什么持续项目工作需要持久职责、显式状态、真实工作区和事件驱动等待。
  <!-- mdorigin:index kind=article -->

- [Holon 的 WorkItem 架构：把工作状态从对话中分离](./why-work-items.md)
  从新版接口上线到旧版下线，解释 WorkItem 与 Goal、Loop 的侧重点：把跨发布周期的目标、进度、外部依赖和交付归入一项持续负责的工作。中文审阅稿，未发布。
  <!-- mdorigin:index kind=article -->

- [从 Issue 到发布：工作不会停在某个人的终端里](./project-work-that-survives-ci-waits.md)
  一个匿名化团队实例展示了怎样的持续 Agent 工作，以及它尚未证明什么。
  <!-- mdorigin:index kind=article -->

- [不止审一次代码：用 Holon 搭建持续跟进 PR 的 Reviewer](./one-pr-one-work-item.md)
  从模板创建自带工作规范与 Skills 的 reviewer，确认职责与合并权限，订阅仓库 PR，自动审阅并持续跟进修复与 CI。
  <!-- mdorigin:index kind=article -->

- [用 Holon 构建你的第一个长期 Agent 工作流](./first-long-lived-agent-workflow.md)
  从安装开始，建立具名 Agent、真实工作区、显式 WorkItem 和人工审批边界。
  <!-- mdorigin:index kind=article -->

- [从个人 AI 工具到团队协作：一个小团队的 Agent Native 实践](./agents-in-a-small-team.md)
  从开发者各自使用 AI，到共享 Agent 参与团队分工：用 77 天的留存用量、设备故障调查和琴键无声的实测案例，记录一个小团队的 Agent Native 实践。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
