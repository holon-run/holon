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

- [在 Web GUI 中从模板创建首个专用 Agent 并跑通测试任务](./create-agent.md)
  在 Web GUI 中审阅并实例化角色模板，确认独立 agent_home 与预置 Skill 到位，并用一个只读测试任务完成初始化验收。
  <!-- mdorigin:index kind=article -->

- [使用 Web GUI](./use-web-gui.md)
  在浏览器里驱动 Agent、Work Item 和 Skill。
  <!-- mdorigin:index kind=article -->

- [创建并定制专属角色 Agent 与权限边界](./customize-agent.md)
  在 Web GUI 中从模板创建或打开已有角色 Agent，通过自然语言对话将常驻职责、自主权限与审批门禁持久化写入 AGENTS.md 和操作者记忆，并以只读演练核验边界生效。
  <!-- mdorigin:index kind=article -->

- [创建 Agent 模板](./create-agent-template.md)
  编写角色契约，添加可选的 Skill 依赖，并通过新 Agent 验证其有效性。
  <!-- mdorigin:index kind=article -->

- [安装、启用与更新 Skill](./use-skills.md)
  管理共享 Skill Library 并核实指定 Agent 的可用 Skill 列表。
  <!-- mdorigin:index kind=article -->

- [编写与验证 Skill](./write-skill.md)
  编写轻量级 SKILL.md 工作流，为测试 Agent 启用并验证其加载与遵循情况。
  <!-- mdorigin:index kind=article -->

- [在项目工作区与 Git Worktree 中工作](./work-with-workspaces.md)
  在对话中直接挂载本地代码仓库、在受控 Git Worktree 中隔离编码改动并完成本地验证，安全清理临时工作树而不污染主工作区。
  <!-- mdorigin:index kind=article -->

- [在自己的电脑上运行常驻 AI 助手](./run-always-on-local-assistant.md)
  把已经在用的本地模型从一次性对话升级为常驻助手：WorkItem 让任务状态跨会话存续，定时唤醒让工作按计划推进，并从手机或其他设备验收结果。
  <!-- mdorigin:index kind=article -->

- [运行长生命周期任务](./run-long-lived-task.md)
  在关闭终端后让 Agent 继续工作，等待外部依赖并在新会话中找回同一任务结果。
  <!-- mdorigin:index kind=article -->

- [将复杂任务安全委派给子 Agent 并验收交付](./delegate-work.md)
  在自然语言对话中让主 Agent 将耗时审计、独立测试或试验性重构委派给隔离的子 Agent，配置严格的只读或独立 Worktree 分支边界，并在验收产物后安全清理实例。
  <!-- mdorigin:index kind=article -->

- [搭建 AI Agent 生态研究员：跨多开源项目的持续深度调研](./build-agent-ecosystem-researcher.md)
  复用 Research Steward 模板与多 Workspace 机制。深入剖析 Agent 自动维护知识库的底层原理，展示让 Agent 自动克隆挂载仓库、精简指令交互，以及移动端随手转发链接的异步调研工作流。
  <!-- mdorigin:index kind=article -->

- [用 research-steward 模板构建可溯源的调研与决策简报工作流](./build-research-steward.md)
  在 Web GUI 中从 research-steward 模板创建专用调研 Agent，通过自然语言驱动问题界定、一手来源台账、引用核验与结构化决策简报生成，并跨会话跟进待核验队列。
  <!-- mdorigin:index kind=article -->

- [搭建持续跟进 PR 的本地 Code Reviewer](./build-code-reviewer.md)
  从 code-reviewer 模板创建审阅 Agent，直接通过对话让它挂载代码库、记录合并权限并订阅 GitHub 事件，持续跟进新提交与 CI 直至合并。
  <!-- mdorigin:index kind=article -->

- [搭建持续同步代码与文档的本地 Docs Steward](./build-docs-steward.md)
  从 docs-steward 模板创建文档维护 Agent，通过对话让它挂载代码仓库、审计代码与文档的不一致漂移，并在独立 Git worktree 中提交带有证据引用的纯文档 PR。
  <!-- mdorigin:index kind=article -->

- [为不同角色规划与隔离 Agent Skills](./organize-agent-skills-and-roles.md)
  避免工具泛滥与指令互斥：通过自然语言为不同长期 Agent 分配专属技能、工作边界与权限，保持上下文干净。
  <!-- mdorigin:index kind=article -->

- [用 Holon 一次跑通或持续跟进 GitHub Issue 与 PR 修复](./run-github-task.md)
  根据场景选择 Web GUI 对话式工作流（适合多轮 PR 反馈、CI 等待与独立 worktree 隔离）或单次执行的 holon solve（适合 CLI 与 CI 批处理），明确前置核验、发布门禁与收尾清理。
  <!-- mdorigin:index kind=article -->

- [为 Agent 配置外部事件 Webhook：从 GitHub 到即时通讯通知响应](./connect-external-events.md)
  在自然语言对话中让 Agent 配置外部 Webhook 回调，利用事件驱动与唤醒机制实现零 Token 挂起等待，并在收到外部通知后安全核验与闭环执行。
  <!-- mdorigin:index kind=article -->

- [用 Agent Inbox 统一收口外部通知与异步事件](./connect-event-inbox.md)
  通过 AgentInbox 统一订阅 GitHub、邮件与飞书等多源外部通知，以低噪 wake_hint 唤醒 Agent，在权限边界内分批拉取、去重审阅并清理临时订阅。
  <!-- mdorigin:index kind=article -->

- [调度 Agent 工作](./schedule-agent-work.md)
  定时执行已授权的只读检查，验证返回的工作结果并及时取消定时器。
  <!-- mdorigin:index kind=article -->

- [通过 HTTP 自动化执行](./automate-over-http.md)
  鉴权接入、提交单一跟踪目标、跟进执行状态并获取完成报告。
  <!-- mdorigin:index kind=article -->

- [配置与切换大模型：从本地 Ollama 到多云灾备](./configure-models.md)
  从本地零成本离线运行 Ollama 到云端主流模型接入，在对话中随时自检生效模型，并配置自动多云灾备回退链保障高可用。
  <!-- mdorigin:index kind=article -->

- [在离线与自托管环境中运行私有 Agent：从 Ollama 到私有搜索与本地工具链](./run-offline-private-agent.md)
  把 Holon 运行时部署在纯断网离线或内网自托管环境中：锁定本地 Ollama/vLLM 模型与禁用云端回退，配置 SearXNG 私有搜索或命令型本地检索、WebFetch 内网白名单、本机工具链边界，并在对话中完成验证。
  <!-- mdorigin:index kind=article -->

- [配置咨询决策子系统](./configure-decision-subsystem.md)
  选择决策模型路由、设置调用限额，并验证决策建议或显式弃权行为。
  <!-- mdorigin:index kind=article -->

- [管理 Agent 记忆：从长期偏好沉淀、原始历史检索到过期记忆核验与纠偏](./manage-agent-memory.md)
  在自然语言对话中让 Agent 沉淀操作偏好与长期事实，利用强原始历史检索避免信息丢失与幻觉，并在环境变更时核验与修剪过期矛盾记忆。
  <!-- mdorigin:index kind=article -->

- [连接到远程 Holon 运行时](./connect-remote-runtime.md)
  从另一台电脑或手机访问远程运行时，验证连接并查回已有任务结果。
  <!-- mdorigin:index kind=article -->

- [配置 OIDC 认证](./configure-oidc-authentication.md)
  配置 OpenID Connect 单点登录、团队共享运行时提示词审计与会话超时策略。
  <!-- mdorigin:index kind=article -->

- [用 Google Workspace 配置 OIDC 单点登录](./google-workspace-oidc-authentication.md)
  在 Google Cloud 中创建 OAuth 客户端，把 Google Workspace 作为 Holon 的身份提供商，并完成组织准入、配置与验证。
  <!-- mdorigin:index kind=article -->

- [用自托管 GitLab CE 配置 OIDC 单点登录](./gitlab-ce-oidc-authentication.md)
  自托管 GitLab CE 作为 Holon 的 OIDC 身份提供商：部署实例、创建 OAuth 应用、配置 Holon 并完成端到端验证，全程不依赖任何外部账号。
  <!-- mdorigin:index kind=article -->

- [用自托管 Keycloak 配置 OIDC 单点登录](./keycloak-oidc-authentication.md)
  自托管 Keycloak 作为 Holon 的 OIDC 身份提供商：Docker 部署、创建 realm 与客户端、配置 Holon 并完成端到端验证，全程不依赖任何外部账号。
  <!-- mdorigin:index kind=article -->

- [用自托管 Casdoor 配置 OIDC 单点登录](./casdoor-oidc-authentication.md)
  自托管 Casdoor 作为 Holon 的 OIDC 身份提供商：Docker 部署、初始化组织与应用、配置 Holon 并完成端到端验证，全程不依赖任何外部账号。
  <!-- mdorigin:index kind=article -->

- [用自托管 changedetection.io 让网页变更唤醒 Holon agent](./changedetection-webhook-setup.md)
  自托管 changedetection.io 监控任意网页，检出变更后通过 webhook 回调唤醒 Holon agent：Docker 部署、watch 与通知配置、端到端验证，全程本地闭环。
  <!-- mdorigin:index kind=article -->

- [用自托管 GlitchTip 把错误告警唤醒 Holon](./glitchtip-error-alerts.md)
  自托管 GlitchTip 作为错误追踪：错误经 Sentry 兼容摄入生成 issue，项目告警 webhook 唤醒 Holon，agent 再用 REST API 核对 issue 与事件。不改 Holon 运行时代码。
  <!-- mdorigin:index kind=article -->

- [与团队共用 AI 助手](./share-with-your-team.md)
  在共享服务器部署 Holon、配置团队单点登录，全组协同交办任务给同一个长期 AI 助手并跟踪进度。
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

- [用自托管 SearXNG 配置 Holon WebSearch 搜索后端](./searxng-websearch-provider.md)
  使用 Docker 自托管 SearXNG 开源元搜索引擎，为 Holon 提供免云端凭据的 WebSearch 本地搜索后端：容器部署调优、配置接入与端到端验证排错。
  <!-- mdorigin:index kind=article -->

- [生成图片](./generate-image.md)
  编写提示词、生成图片并在磁盘中定位产物。
  <!-- mdorigin:index kind=article -->

- [用视觉工具检查图片](./inspect-image.md)
  让视觉模型观察本地图片并解读画面内容。
  <!-- mdorigin:index kind=article -->

- [为 Agent 搭建与托管本地交互应用](./host-local-apps.md)
  在对话中直接让 Agent 在其 apps 目录下创建可视化任务看板，使用 window.Holon.request() 提交结构化请求并实时接收队列事件，无需手动查找目录或执行 CLI 命令。
  <!-- mdorigin:index kind=article -->

- [观察 Agent 工作](./observe-agent-work.md)
  定位 Agent 的工作目标、查看任务进展并获取最终完成报告与交付产物。
  <!-- mdorigin:index kind=article -->

- [排查卡住或无响应的 Agent 任务](./troubleshooting.md)
  在 Web GUI 对话中优先自检卡住、长时间无输出或失败的 Agent 任务：区分正常的 WaitFor 挂起等待与真实阻塞，逐层核查 TaskStatus 与子 Agent 状态、模型路由、工作树冲突及守护进程故障，并安全恢复推进。
  <!-- mdorigin:index kind=article -->

- [配置可观测性](./configure-observability.md)
  导出追踪 Trace、抓取受保护的指标端点，并将遥测通道与业务执行独立核验。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
