---
title: 指南
summary: 完成特定任务、验证交付成果并排查常见问题的操作指南。
order: 30
---

# 指南

根据你想要达成的目标选择对应的指南。每篇指南详细列出前置条件、操作步骤与核验方法。如果你还没有成功运行过任何请求，建议先从[开始使用](../getting-started/)开始。

目录编排遵循实际工作的推进顺序：首先配置 Agent 与扩展能力，发起并跟进长期任务，配置模型与记忆系统，连接客户端，然后处理网页内容、图像与 Local App。最后的指南介绍如何检查交付成果、排查疑难问题以及收集运行时遥测数据。

阅读[概念](../concepts/)了解核心对象与系统边界；查阅[参考](../reference/)获取完整命令语法与 Schema 契约。

<!-- INDEX:START -->

- [从模板创建 Agent](./create-agent.md)
  挑选模板、创建 Agent 并确认其能正常接收处理任务。
  <!-- mdorigin:index kind=article -->

- [使用 Web GUI](./use-web-gui.md)
  在浏览器里驱动 Agent、Work Item 和 Skill。
  <!-- mdorigin:index kind=article -->

- [定制 Agent 角色与指引](./customize-agent.md)
  直接更新现有 Agent 的常驻指令，无需替换其 Home 目录或重新套用模板。
  <!-- mdorigin:index kind=article -->

- [创建 Agent 模板](./create-agent-template.md)
  编写角色契约，添加可选的 Skill 依赖，并通过新 Agent 验证其有效性。
  <!-- mdorigin:index kind=article -->

- [排查 Holon 任务问题](./troubleshooting.md)
  把卡住、失败或无输出的任务定位到一个明确的下一步。
  <!-- mdorigin:index kind=article -->

- [安装、启用与更新 Skill](./use-skills.md)
  管理共享 Skill Library 并核实指定 Agent 的可用 Skill 列表。
  <!-- mdorigin:index kind=article -->

- [编写与验证 Skill](./write-skill.md)
  编写轻量级 SKILL.md 工作流，为测试 Agent 启用并验证其加载与遵循情况。
  <!-- mdorigin:index kind=article -->

- [在项目工作区或 worktree 中工作](./work-with-workspaces.md)
  挂载代码项目、激活执行根、在受控 worktree 中隔离改动并安全清理。
  <!-- mdorigin:index kind=article -->

- [运行长生命周期任务](./run-long-lived-task.md)
  跨会话跟踪单一目标、等待外部依赖并在验收完成后核对结果摘要。
  <!-- mdorigin:index kind=article -->

- [将工作委派给另一个 Agent](./delegate-work.md)
  挑选子 Agent 或对等 Agent、跟进其回复、验证工作结果并安全清理下属实例。
  <!-- mdorigin:index kind=article -->

- [使用 holon solve 处理 GitHub 任务](./run-github-task.md)
  明确 issue、评审或 PR 修复的目标与交付边界；单次执行并验证本地产物与 GitHub 副作用。
  <!-- mdorigin:index kind=article -->

- [连接外部事件](./connect-external-events.md)
  配置回调端点、登记等待条件，并验证外部事件能够如期唤醒目标工作。
  <!-- mdorigin:index kind=article -->

- [调度 Agent 工作](./schedule-agent-work.md)
  创建单次或周期性定时器，验证定时触发的工作并及时取消调度。
  <!-- mdorigin:index kind=article -->

- [通过 HTTP 自动化执行](./automate-over-http.md)
  鉴权接入、提交单一跟踪目标、跟进执行状态并获取完成报告。
  <!-- mdorigin:index kind=article -->

- [配置模型、凭据与回退](./configure-models.md)
  配置服务商凭据、指定全局或 Agent 级路由，并验证单次交互中使用的实际路由。
  <!-- mdorigin:index kind=article -->

- [配置咨询决策子系统](./configure-decision-subsystem.md)
  选择决策模型路由、设置调用限额，并验证决策建议或显式弃权行为。
  <!-- mdorigin:index kind=article -->

- [管理 Agent 记忆](./manage-agent-memory.md)
  在正确的作用域中保存长期偏好，并通过可追溯的来源检索历史工作记录。
  <!-- mdorigin:index kind=article -->

- [连接到远程 Holon 运行时](./connect-remote-runtime.md)
  访问部署在其他机器上的运行时并验证连接安全性。
  <!-- mdorigin:index kind=article -->

- [配置 OIDC 认证](./configure-oidc-authentication.md)
  配置 OpenID Connect 单点登录、会话超时策略及用户提示词审计。
  <!-- mdorigin:index kind=article -->

- [使用 Android 客户端连接](./connect-android-client.md)
  配置原生 Android 客户端，与运行中的 Holon 守护进程配对，并在移动设备上管理 Agent。
  <!-- mdorigin:index kind=article -->

- [使用 iOS 客户端连接](./connect-ios-client.md)
  配置原生 iOS 客户端，与运行中的 Holon 守护进程配对，在 iPhone 和 iPad 上查看产物与文件并举报 AI 内容。
  <!-- mdorigin:index kind=article -->

- [获取与搜索网络内容](./use-web-tools.md)
  在搜索与抓取之间做取舍，并在保持审慎的前提下阅读外部网页产物。
  <!-- mdorigin:index kind=article -->

- [生成图片](./generate-image.md)
  编写提示词、生成图片并在磁盘中定位产物。
  <!-- mdorigin:index kind=article -->

- [用视觉工具检查图片](./inspect-image.md)
  让视觉模型观察本地图片并解读画面内容。
  <!-- mdorigin:index kind=article -->

- [托管本地应用](./host-local-apps.md)
  构建一个由 Agent 拥有的轻量本地应用，提交请求并在浏览器中验证队列事件。
  <!-- mdorigin:index kind=article -->

- [观察 Agent 工作](./observe-agent-work.md)
  定位 Agent 的工作目标、查看任务进展并检索最终完成报告与交付产物。
  <!-- mdorigin:index kind=article -->

- [配置可观测性](./configure-observability.md)
  导出追踪 Trace、抓取受保护的指标端点，并将遥测通道与业务执行独立核验。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
