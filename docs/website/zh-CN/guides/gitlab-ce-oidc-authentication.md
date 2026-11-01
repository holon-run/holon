---
title: 用自托管 GitLab CE 配置 OIDC 单点登录
summary: 自托管 GitLab CE 作为 Holon 的 OIDC 身份提供商：部署实例、创建 OAuth 应用、配置 Holon 并完成端到端验证，全程不依赖任何外部账号。
order: 186
---

# 用自托管 GitLab CE 配置 OIDC 单点登录

自托管的 GitLab CE（Free 层即可）可以作为 Holon 的 OIDC 身份提供商：用户、OAuth 应用、客户端密钥全部在你自己的 GitLab 实例里创建与管理，不需要向任何第三方注册。本文给出从部署实例到端到端验证的完整流程，全部步骤在一台机器上即可复现。

通用概念（Issuer、Client ID、回调地址、会话 TTL、审计归属）见[配置 OIDC 认证](./configure-oidc-authentication.md)。本文只补充自托管 GitLab 特有的部分。

## 前置条件

- 一台可以跑 Docker 的主机（GitLab CE 容器约需 4 GB 内存、15–20 GB 磁盘）。
- GitLab 站点必须以 HTTPS 对外提供。Holon 的 OIDC 客户端只信任内置的 Mozilla 根证书，**自签名 CA 不可行**；内网环境推荐用 Tailscale 的 `tailscale cert` 签发受公信 CA（Let's Encrypt）背书的证书，无需公网暴露端口。
- 一个可访问的 Holon 实例。回调地址允许 HTTPS，或主机名严格为 `localhost` 的 HTTP（端口任意）。`127.0.0.1`、`::1` 这类 IP 字面量会被拒绝。

## 第 1 步：部署 GitLab CE

用 Docker 启动，把证书挂载进容器并关闭内置 Let's Encrypt：

```bash
mkdir -p gitlab/{config,logs,data}
docker run -d --name gitlab --shm-size 256m \
  -p 9443:443 \
  -v "$PWD/gitlab/config:/etc/gitlab" \
  -v "$PWD/gitlab/logs:/var/log/gitlab" \
  -v "$PWD/gitlab/data:/var/opt/gitlab" \
  -v "$PWD/certs/site.crt:/etc/gitlab/ssl/site.crt:ro" \
  -v "$PWD/certs/site.key:/etc/gitlab/ssl/site.key:ro" \
  -e GITLAB_ROOT_PASSWORD="$(openssl rand -hex 16)" \
  -e GITLAB_OMNIBUS_CONFIG="external_url 'https://gitlab.example.com:9443'; nginx['listen_port'] = 443; nginx['listen_https'] = true; nginx['ssl_certificate'] = '/etc/gitlab/ssl/site.crt'; nginx['ssl_certificate_key'] = '/etc/gitlab/ssl/site.key'; letsencrypt['enable'] = false; prometheus_monitoring['enable'] = false" \
  gitlab/gitlab-ce:latest
```

首次启动需要几分钟完成初始化。就绪判定：`curl https://gitlab.example.com:9443/.well-known/openid-configuration` 返回 200，且 `issuer` 字段与站点 URL（含端口）一致。

注意：`external_url` 带非 443 端口时，证书仍然是普通域名证书（证书验证不含端口），发现文档与 issuer 会自动带上端口号。

## 第 2 步：创建 OAuth 应用

在 GitLab 中创建机密（confidential）OAuth 应用：

1. 用户头像 → **Edit profile** → **Applications** → **New application**（实例级应用在 Admin 区创建）。
2. Name 任意（如 `holon`）；**Redirect URI** 填 Holon 的回调地址，例如 `http://localhost:7879/api/auth/oidc/callback`。
3. Scopes 勾选 **openid**、**profile**、**email**。
4. **Confidential** 保持勾选（GitLab 18 起数据库层强制机密客户端）。
5. 保存后记录 **Application ID** 与 **Secret**。

scope 说明：`openid` 是签发 id_token 的必要条件（缺它拿不到 id_token）；`email`、`profile` 提供 Holon 展示用的邮箱与显示名。如果需要在 id_token 中携带用户组，还要额外注册并请求 `groups` scope，本流程默认不启用。

## 第 3 步：创建用户并确认邮箱

管理员在 Admin 区创建用户，或让用户自注册。**一个容易踩的坑**：GitLab 要求用户完成邮箱确认后才能完成 OAuth 授权——未确认邮箱的用户登录成功，但授权页会报「Verify the email address in your account」。测试实例可以由管理员直接确认，或在设置中放宽确认要求。

## 第 4 步：配置 Holon

```bash
holon config set auth.oidc.issuer_url https://gitlab.example.com:9443
holon config set auth.oidc.client_id <Application ID>
holon config set auth.oidc.client_secret_env GITLAB_OIDC_CLIENT_SECRET
holon config set auth.oidc.redirect_uri http://localhost:7879/api/auth/oidc/callback
holon config set auth.mode oidc
```

`GITLAB_OIDC_CLIENT_SECRET` 是环境变量名，密钥值在启动 Holon 时注入，不写入配置文件：

```bash
GITLAB_OIDC_CLIENT_SECRET="<Application Secret>" holon serve --port 7879
```

已知限制：当前版本 `holon config set` 每次写入后都会对整份认证配置做校验，因此**先设置 `auth.mode oidc` 再补 `auth.oidc.*` 子键会被拒绝**（处于既不能是 oidc 也不能是 local 的中间状态）。按本文顺序（先子键后 mode）可以避开；如果已经设置过 mode，直接编辑 `config.json` 后重启即可。

## 第 5 步：验证

1. 打开 Holon 登录页，应出现「Continue with organization login」；点击后跳转 GitLab 登录。
2. 登录 GitLab 并完成授权，回到 Holon 后检查会话：

```bash
curl -s http://localhost:7879/api/auth/session/me
# {"ok":true,"user_id":"oidc-…","display_name":"…","auth_method":"oidc"}
```

3. 登出后同一路径应返回 401：

```bash
curl -s -X POST http://localhost:7879/api/auth/session/logout
```

反向检查：错误 client_secret 时 token 端点返回 401 `invalid_client`；未注册的 redirect_uri 拿不到授权码；scope 缺 `openid` 时没有 id_token。

## 与 Google Workspace 的差异

| 维度 | 自托管 GitLab CE | Google Workspace |
| --- | --- | --- |
| 外部依赖 | 无（全部本地） | 需要 Google Cloud 项目与组织 |
| 组织准入 | GitLab 用户/组管理（不邀请的人不存在） | OAuth 同意屏幕 User type = Internal + `hd` 域（Holon 侧不校验 `hd`） |
| 单点登出 | discovery 无 `end_session_endpoint`，Holon 登出只结束本地会话 | 同样不由 Holon 强制 |
| 用户组 claims | 需注册并请求 `groups` scope | 不适用 |

## 本文所述的验证程度

以下结论来自 2026-11-01 的实测（GitLab CE 官方 Docker 镜像 + 隔离 Holon 实例，issuer 为 `https://<tailscale 主机名>:9443`）：

- 已实测：程序化 code flow 全链（PKCE S256、state、nonce）在 GitLab 与隔离 Holon 实例之间完成登录并建立会话；id_token 通过本地重建 JWKS 公钥完成 RS256 验签，`iss`/`aud`/`exp`/`iat`/`nonce`/`sub` 合规，userinfo 的 `sub` 一致；错误 client_secret 被拒（401 `invalid_client`）、未注册 redirect_uri 拿不到授权码、缺 `openid` scope 无 id_token；`/api/auth/session/me` 与登出行为符合预期。
- 未实测：浏览器手动点击的完整 UI 流程（验证以程序化 HTTP 流为准）、LDAP/企业级用户目录、`groups` scope 启用后的组同步、GitLab.com SaaS、生产规模的部署与备份恢复。
