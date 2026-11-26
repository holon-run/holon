---
title: 与团队共用 AI 助手
summary: 在共享服务器部署 Holon、配置团队单点登录，全组协同交办任务给同一个长期 AI 助手并跟踪进度。
order: 185
---

# 与团队共用 AI 助手

当你已经在个人电脑上跑通了 Holon 之后，下一步往往是把它分享给团队。与其让每位同事都在本地各配一套环境、互相复制 Prompt，不如在一台常开的服务器上部署 Holon，相当于全组共雇一位常驻待命的 AI 同事：它记住项目的长期上下文与协作规范，团队成员都可以交办任务，离开电脑后它继续跟进，并在完成后交付清晰的结果简报。

本指南介绍实现团队协作的三步落地路径，以及日常协同的最佳实践。

## 为什么要与团队共用 AI 助手？

相比各自在个人电脑上运行孤立的对话工具，团队共用 AI 助手有三个核心优势：

1. **共享上下文与项目规范：** 助手常驻在团队的 canonical 代码库或共享工作区中，读取统一的 `AGENTS.md` 规则与知识库，不再出现“某人在本地调通、别人问就答错”的环境与指令偏差。
2. **交办明确任务与审计可溯：** 任何团队成员都可以为助手创建带有明确验收标准的独立任务（WorkItem）。通过接入企业 OIDC 单点登录，Holon 会精准记录每项任务和指令是由哪位同事发起的。
3. **离线自动跟进与交付：** 当任务需要等待耗时的 CI 构建、测试运行、外部下载或人工审批时，同事可以随时关机离开。助手在依赖就绪后会自动唤醒继续执行，并在最终交付时提供提炼好的简报（Brief）。

## 第 1 步：部署到团队共享服务器

挑选一台全组均可网络访问的机器——例如云服务器、办公室固定工作站或 NAS。

安装 Holon 后，根据团队现有的网络架构启动守护进程：

- **Cloudflare Named Tunnel（`--access cloudflare`）：** 使用预配置的专用隧道、域名与 connector token 提供公网 HTTPS。
- **Tailscale（`--access tailnet`）：** 适合团队成员已加入同一私有虚拟网格的场景。
- **局域网或反向代理（`--access lan`）：** 适合已有自建机房，前端挂载 Nginx、Caddy 或 Kubernetes Ingress 的场景。

```bash
# 先配置 Named Tunnel ingress 指向 http://127.0.0.1:8787。
holon daemon start --access cloudflare --port 8787 \
  --advertise https://holon.example.com \
  --cloudflare-token-file ~/.cloudflared/holon-connector.token \
  --token-file ~/.holon/remote.token
```

有关详细的网络配置与服务管理说明，请参阅[连接到远程 Holon 运行时](./connect-remote-runtime.md)。

## 第 2 步：接入团队单点登录（OIDC）

在团队成员之间私下分发固定的控制 Token 无法做到安全审计。对于团队环境，建议配置 OpenID Connect（OIDC），让成员直接使用公司现有的账号系统登录（如 Google Workspace、GitLab CE、Keycloak、Casdoor、Okta、Authentik 或 Microsoft Entra ID）。

开启 OIDC 后：
- 成员在浏览器中直接使用工作邮箱或单点登录凭据登录 Web GUI；
- Holon 精确记录发起提示词与交办任务的具体用户身份；
- 成员离职或权限变更时，在企业 IdP 侧一键撤销即可生效。

```bash
# 切换为 OIDC 认证模式并配置基础参数
holon config set auth.mode "oidc"
holon config set auth.oidc.issuer_url "https://auth.example.com/realms/team"
holon config set auth.oidc.client_id "holon"
holon config set auth.oidc.client_secret_env "HOLON_OIDC_CLIENT_SECRET"
```

完整的分步注册与会话超时调整指引，请参阅[配置 OIDC 认证](./configure-oidc-authentication.md)，或直接参考针对 [Google Workspace](./google-workspace-oidc-authentication.md)、[自托管 GitLab CE](./gitlab-ce-oidc-authentication.md)、[自托管 Keycloak](./keycloak-oidc-authentication.md) 与[自托管 Casdoor](./casdoor-oidc-authentication.md) 的专项配置指南。

## 第 3 步：通过 Web GUI 展开日常协同

部署与认证就绪后，在浏览器中打开服务端对外地址即可进入 [Web GUI](./use-web-gui.md)：

1. **选择专属 Agent：** 挑选专职负责该项目的常驻 Agent（例如负责日常发布协调的 Release Manager、维护文档规范的 Docs Steward 或专职代码审查的 Code Reviewer）。
2. **交办任务（WorkItem）：** 新建一个有独立生命周期的工作项，写清目标和验收边界（例如：“排查并修复 issue #123” 或 “核对本周发布版本的依赖兼容性”）。
3. **查看进度与接力协作：** 全组成员都能在任务列表中看到当前正在进行的工作项、执行日志与中间产物，其他同事可以在此基础上补充输入或接力推进。
4. **审阅完成简报：** 任务闭环后，助手会生成结构化完成简报，列出核心结论、已核验项及需要人工最终确认的决策点。

如需了解实际研发团队在真实项目中如何与 AI 助手协同工作，建议阅读博客实录：
[一个小团队如何与长期 AI 助手协同工作——131 天真实实践](/zh-CN/blog/agents-in-a-small-team)。

## 日常协同最佳实践

部署只是让助手跑起来，下面三个习惯决定团队是真的在共用它，还是慢慢退回到各开各的对话窗口。

### 把验收标准写进 WorkItem

团队里任何成员都可以给助手交办任务。让任务能被共用的，是它自带“完成”的定义：目标是什么、边界在哪里（哪些文件或系统不在范围内）、结果按什么判据验收。

```text
升级依赖 X，改动只保留在清单文件和 lockfile 中，不要顺手格式化无关代码。
验收：完整检查通过，且 CI 工作流为绿。
```

只存在于聊天消息里的交代，会随着对话滚屏一起消失。被跟踪的工作项会把计划、检查清单与验收标准保留下来，下一位接手的人（包括一周之后交办人自己）都还能读到。

### 接力推进，而不是重开一个任务

当助手停下来等待时，先看记录下来的等待原因，再决定要不要发言。它可能在等你决策、等后台任务结果，或在等 CI、评审这类外部事件。如果是在等你决策，回答那个问题就等于让同一个工作项继续推进，不需要新建任务，也不用重新交代一遍上下文。

助手仍在工作时发出的请求会作为消息排队给它，不会丢弃正在进行的那一轮。你不需要先等它结束才能补充信息。

在把某件事当成“完成”之前，区分两个层级：

- **Task 完成**：某条命令、某次构建或某次子 Agent 调用返回了结果，这是证据。
- **WorkItem 完成**：目标与验收标准都已满足。构建变绿并不等于改动已被评审。

### 在身份提供方撤销访问，历史照常保留

启用 OIDC 后，每条提示词都会记录认证用户的稳定 `actor_id`，以及发送当时抓取的 `actor_display_name`。因此历史记录是自洽的：即使该成员后来改名，历史仍保留当时是谁发起的；有人离开也不需要回头改写任何记录。

成员离开项目时，在企业 IdP 侧停用其账号即可，这是终止访问的那一步。

OIDC 回答的是“这件事是谁发起的”，它不会在成员之间隔离文件或工具输出：共用同一个运行时的成员面对的是同一批工作区。所以访问控制交给身份提供方，责任归属交给工作项，不要指望它带来按用户的隔离。

## 另见

- [工作项与任务](../concepts/work-items-and-tasks.md) — 长期目标与执行任务的区别
- [观察 Agent 工作](./observe-agent-work.md) — 在 Web GUI 中跟踪进度、等待与结果
- [配置 OIDC 认证](./configure-oidc-authentication.md) — IdP 注册、声明与会话有效期
- [连接到远程 Holon 运行时](./connect-remote-runtime.md) — 网络模式、连通性验证与排障
