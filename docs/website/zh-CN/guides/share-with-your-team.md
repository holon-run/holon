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

在团队成员之间私下分发固定的控制 Token 无法做到安全审计。对于团队环境，建议配置 OpenID Connect（OIDC），让成员直接使用公司现有的账号系统登录（如 Google Workspace、GitLab CE、Keycloak、Okta、Authentik 或 Microsoft Entra ID）。

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

完整的分步注册与会话超时调整指引，请参阅[配置 OIDC 认证](./configure-oidc-authentication.md)，或直接参考针对 [Google Workspace](./google-workspace-oidc-authentication.md)、[自托管 GitLab CE](./gitlab-ce-oidc-authentication.md) 与[自托管 Keycloak](./keycloak-oidc-authentication.md) 的专项配置指南。

## 第 3 步：通过 Web GUI 展开日常协同

部署与认证就绪后，在浏览器中打开服务端对外地址即可进入 [Web GUI](./use-web-gui.md)：

1. **选择专属 Agent：** 挑选专职负责该项目的常驻 Agent（例如负责日常发布协调的 Release Manager、维护文档规范的 Docs Steward 或专职代码审查的 Code Reviewer）。
2. **交办任务（WorkItem）：** 新建一个有独立生命周期的工作项，写清目标和验收边界（例如：“排查并修复 issue #123” 或 “核对本周发布版本的依赖兼容性”）。
3. **查看进度与接力协作：** 全组成员都能在任务列表中看到当前正在进行的工作项、执行日志与中间产物，其他同事可以在此基础上补充输入或接力推进。
4. **审阅完成简报：** 任务闭环后，助手会生成结构化完成简报，列出核心结论、已核验项及需要人工最终确认的决策点。

如需了解实际研发团队在真实项目中如何与 AI 助手协同工作，建议阅读博客实录：
[一个小团队如何与长期 AI 助手协同工作——77 天真实实践](/zh-CN/blog/agents-in-a-small-team)。
