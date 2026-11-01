---
title: 用 Google Workspace 配置 OIDC 单点登录
summary: 在 Google Cloud 中创建 OAuth 客户端，把 Google Workspace 作为 Holon 的身份提供商，并完成组织准入、配置与验证。
order: 185
---

# 用 Google Workspace 配置 OIDC 单点登录

Google Workspace 可以作为 Holon 的 OIDC 身份提供商：团队成员用组织账号登录，Holon 据此记录每条提示词的发起者。本文给出从创建应用、配置 Holon 到验证的完整流程，并说明组织准入应当收敛在哪一侧。

通用概念（Issuer、Client ID、回调地址、会话 TTL、审计归属）见[配置 OIDC 认证](./configure-oidc-authentication.md)。本文只补充 Google Workspace 特有的部分。

## 先分清两套凭据

接入 Google Workspace 常见两件事，凭据与风险完全不同，不要混用同一个客户端：

| 目标 | 所需客户端 | 需要的 API | 凭据长期性 |
| --- | --- | --- | --- |
| **用户登录 Holon**（本文） | Web application（机密客户端） | 不需要启用任何 Workspace API，仅 `openid`、`profile`、`email` | 每次登录重新签发，不依赖 refresh token |
| Agent 代用户读写 Gmail / Calendar / Drive | 独立客户端 + `offline` 授权 | 需启用对应 API | 依赖 refresh token，受同意屏幕 User type 影响 |

本文只覆盖第一行。第二行属于 Agent 的 Google API 集成，通常应使用单独的客户端、单独的授权范围，并与登录客户端隔离，避免登录凭据被扩展成数据访问权限。

## 前置条件

- Google Workspace 组织的管理员权限（能修改 OAuth 同意屏幕与 API 访问控制）。
- 一个可访问的 Holon 地址。生产环境必须 HTTPS；仅当主机名严格为 `localhost` 时回调允许 HTTP。
- 本文示例使用 `example.com` 与 `holon.example.com`，请替换成自己的域名。

## 第 1 步：创建归属组织的 Cloud 项目

在 [Google Cloud Console](https://console.cloud.google.com) 新建项目，并在创建时把组织选为你的 Workspace 组织。等价命令行：

```bash
gcloud projects create holon-sso --name="Holon SSO" --organization=<ORG_ID>
```

组织归属决定了下一步能否选择 **Internal** 用户类型，这是整个流程最关键的一步。个人 Gmail 账号创建的项目不属于任何 Workspace 组织，无法选择 Internal。

OIDC 登录不需要启用任何 Workspace API。如果后续要做 Gmail / Calendar 集成，那是另一个客户端的事。

## 第 2 步：配置 OAuth 同意屏幕（User type = Internal）

在 **APIs & Services → OAuth consent screen** 中：

1. 填写应用名称与支持邮箱。
2. **User type 选择 Internal（组织内部用户）**。
3. 保存。

选择 Internal 的两个直接后果：

- **免 Google 应用验证**：只有组织内用户在登录，不需要提交 Google 审核。
- **不受 External + Testing 状态的 refresh token 7 天失效限制**：Testing 状态下的外部应用，其 refresh token 通常 7 天后失效，团队账号会周期性掉线。

> 不要把官方网站的示例写成 External + Testing。那套配置只适合短期试验，长期运行会在登录体验和数据集成上反复出问题。

## 第 3 步：创建 OAuth 客户端（Web application）

在 **APIs & Services → Credentials → Create credentials → OAuth client ID**：

| 字段 | 值 |
| --- | --- |
| Application type | **Web application** |
| Name | `holon`（任意） |
| Authorized redirect URIs | `https://holon.example.com/api/auth/oidc/callback` |

本地 `7878` 端口测试时再追加一条：`http://localhost:7878/api/auth/oidc/callback`。

保存后记录 **Client ID**（形如 `1234567890-xxxx.apps.googleusercontent.com`）与 **Client secret**。

关于客户端类型的两个取舍：

- Holon 走授权码流程并提交 `client_secret`，需要真正的机密客户端，**Web application** 是正确选择。
- Desktop（installed app）类型只适合本机回环测试：它的 secret 按 Google 的定义不作为机密保护，且 `gcloud` 无法创建通用 Desktop 客户端（只能通过控制台创建）。

## 第 4 步：在 Google 侧收敛组织准入

Holon 只负责"这次登录是谁"，准入边界由身份提供商给出。Google Workspace 有两层：

1. **User type = Internal**：非组织账号无法完成授权。这是最低要求。
2. **Admin console 应用访问控制**（推荐）：管理员在 **安全性 → API 控制 → 应用访问控制** 中新增该应用（填 Client ID），策略设为"受信任"或"限制访问"，只对指定组织单元放行。

关于 `hd` 声明：Workspace 账号的 id_token 会带 `hd=<你的域名>` 与 `email_verified=true`，这是可校验信号。但 **Holon 当前不读取 `hd`**——它申请 `openid profile email`，校验 `iss`、`aud`、`exp`、`iat`、`nonce` 与签名，不做域名白名单。因此不要暗示"在 Holon 配置里填域名即可限制组织"，该项只能落在 Google 侧。

## 第 5 步：配置 Holon

**Issuer URL 使用 `https://accounts.google.com`**（不带结尾斜杠）。Google 的发现文档位于 `https://accounts.google.com/.well-known/openid-configuration`，其中 `issuer` 就是该值，必须与之逐字节一致。

### 1. 注入 Client Secret

不要把 secret 写进配置文件或提交到版本库，用环境变量注入守护进程：

```bash
export HOLON_OIDC_CLIENT_SECRET="<client-secret>"
```

systemd 或容器部署时，通过服务定义的 secret 注入或环境配置注入，并收紧该文件的读取权限。在无关的 Shell 会话里 `export` 不会影响已在运行的服务。

### 2. 设置认证参数

```bash
holon config set auth.mode "oidc"
holon config set auth.oidc.issuer_url "https://accounts.google.com"
holon config set auth.oidc.client_id "1234567890-xxxx.apps.googleusercontent.com"
holon config set auth.oidc.client_secret_env "HOLON_OIDC_CLIENT_SECRET"
holon config set auth.oidc.redirect_uri "https://holon.example.com/api/auth/oidc/callback"
```

`auth.oidc.redirect_uri` 必须与授权重定向 URI **逐字节一致**（协议、主机名、端口、路径都要一致），否则 Google 直接返回 `redirect_uri_mismatch`。

### 3. 设置会话策略

```bash
# 空闲超时（秒），期间有合法交互会刷新计时；默认 604800（7 天）
holon config set auth.session.idle_ttl_seconds 43200

# 可选绝对上限（秒），必须 >= idle_ttl_seconds
holon config set auth.session.absolute_ttl_seconds 604800
```

### 4. 重启守护进程

```bash
holon daemon restart
```

变更公网主机名时，务必同步更新 Google 控制台里注册的重定向 URI。

## 第 6 步：验证

1. 打开 `https://holon.example.com/login`，页面应出现 **Continue with organization login**。
2. 点击后跳到 Google 登录页，用组织账号完成登录，回跳到 `/api/auth/oidc/callback`，写入 `holon_session` Cookie 并进入控制台。
3. 检查身份：

   ```bash
   curl -b "holon_session=<session-cookie>" https://holon.example.com/api/auth/session/me
   ```

   预期返回 `auth_method: "oidc"` 与形如 `oidc-<uuid-v4>` 的 `user_id`。

4. 检查审计归属：在 OIDC 模式下，每条用户提示词的来源会记录 `actor_id` 与 `actor_display_name`，可通过 `GET /api/agents/{agent_id}/messages` 核对。
5. 确认未鉴权请求被拦截：

   ```bash
   curl -i https://holon.example.com/api/auth/session/me
   ```

   应返回未鉴权错误，而不是用户信息。

建议同时做几条反向用例，确认拒绝逻辑真的在生效：换掉 `aud` 的 token、已过期的 token、签名被篡改的 token、`alg=none`、未知 `kid`，以及用非组织账号登录。Google 侧选 Internal 时，最后一条应在授权阶段就被拦住。

## 故障排查

- **`redirect_uri_mismatch`**：Google 控制台的授权重定向 URI 与 `auth.oidc.redirect_uri` 不一致，或生产环境误用了 `localhost`。
- **`invalid_client` / 无法登录**：Client ID 填错、secret 未注入守护进程、或客户端类型选成了 Desktop。
- **`403 org_internal` 或账号选择器为空**：登录账号不在该 Workspace 组织内，或被 Admin console 的应用访问控制策略拦截。
- **`invalid issuer`**：`issuer_url` 必须是 `https://accounts.google.com`，不要加尾斜杠，也不要用 `https://accounts.google.com/oauth2` 之类的变体。
- **登录成功但身份不对**：调 `/api/auth/session/me` 核对实际登录账号，不要用显示名称推断身份。
- **登出后刷新立即又登录**：Holon 登出只注销自身会话，不清除 Google 的登录态，两者是不同层次。

## 本文所述的验证程度

- **已在真实 Workspace 组织上实测**：发现文档、JWKS 验签、`iss`/`aud`/`azp`/`exp`/`iat`/`nonce` 校验、`hd` 与 `email_verified` 声明、授权码交换与 refresh token 刷新；反向用例（个人 Gmail 无 `hd`、`hd` 不匹配、`email_verified=false`、`aud` 被换、已过期、签名篡改、`alg=none`、未知 `kid`、伪造 refresh token）全部被正确拒绝。
- **测试使用的客户端类型**：Desktop 客户端 + `127.0.0.1` 回环回调，用于验证 Google 侧协议行为，**不是** Holon 网页登录链路。生产应按本文第 3 步使用 Web application 客户端。
- **未实测**：真实 Holon 实例上的 Web 客户端登录端到端、Admin console 应用访问控制策略、`/revoke` 登出联动、组织外账号在 Google 授权阶段被拦下的实际提示。
- 撰写时依据：Google 发现文档（`https://accounts.google.com/.well-known/openid-configuration`）与本项目 2026-11-01 的联调记录。
