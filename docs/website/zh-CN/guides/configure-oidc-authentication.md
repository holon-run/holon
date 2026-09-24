---
title: 配置 OIDC 身份认证
summary: 设置 OpenID Connect 单点登录、配置会话超时策略，并审计用户触发的消息。
order: 17
---

# 配置 OIDC 身份认证

默认情况下，Holon 使用本地 Control Token 进行身份验证。如果需要部署在共享团队服务器或对外提供服务，可以切换为 OpenID Connect (OIDC) 认证。团队成员通过公司现有的身份提供商 (IdP) 登录，Holon 会在每条触发 Agent 的消息中记录具体的操作人身份。

本指南涵盖在 IdP 中注册客户端、配置 Holon 运行时、调整会话超时策略以及验证登录流程。

## 前置准备

- 一个可供用户访问的 Holon 实例（例如通过 `--access tunnel`、`--access tailnet` 或反向代理对外暴露）。
- 一个兼容 OIDC 的身份提供商，如 Keycloak、Okta、Authentik、Google Workspace 或 Microsoft Entra ID。
- 在该 IdP 中创建新应用客户端的管理权限。

> **安全提示：** 在生产环境中，Holon 要求 OIDC Issuer URL 和回调地址必须使用 HTTPS。仅当回调主机为 `localhost` 时才允许使用 HTTP。

## 第一步：在 IdP 中注册 Holon 应用

在你的身份提供商管理后台中新建一个 OpenID Connect 应用：

1. **Client ID**：指定客户端标识符，例如 `holon`。
2. **客户端身份验证（Client Authentication）**：启用机密客户端（Confidential Client）并生成 **Client Secret**。
3. **重定向 URI（Redirect URI）**：配置回调地址：
   ```text
   https://<your-holon-host>/api/auth/oidc/callback
   ```
   如果在本地开发环境测试（端口 7878），使用：
   ```text
   http://localhost:7878/api/auth/oidc/callback
   ```
4. **作用域（Scopes）**：确保客户端至少申请了 `openid`、`profile` 和 `email`。

记录下 IdP 的 **Issuer URL**、**Client ID** 与 **Client Secret**。

## 第二步：将 Client Secret 写入环境变量

不要把凭据密钥硬编码在磁盘上的配置文件中。在 Holon daemon 运行的环境中设置环境变量：

```bash
export HOLON_OIDC_CLIENT_SECRET="your-oidc-client-secret"
```

如果使用 systemd 服务或容器运行 Holon，请将该变量添加到服务单元文件或环境变量配置中。

## 第三步：配置 Holon 运行时

使用 `holon config set` 设置认证模式与提供商参数：

```bash
# 切换认证模式为 OIDC
holon config set auth.mode "oidc"

# 设置 Issuer URL（必须支持 /.well-known/openid-configuration 元数据发现）
holon config set auth.oidc.issuer_url "https://auth.example.com/realms/team"

# 设置已注册的 Client ID
holon config set auth.oidc.client_id "holon"

# 指定存放 Client Secret 的环境变量名
holon config set auth.oidc.client_secret_env "HOLON_OIDC_CLIENT_SECRET"

# 设置公开的回调地址（在反向代理后部署时建议显式设置）
holon config set auth.oidc.redirect_uri "https://holon.example.com/api/auth/oidc/callback"
```

## 第四步：配置会话策略（Session TTL）

Holon 会为浏览器颁发 HttpOnly Session Cookie，为 API 客户端颁发 Session 凭据。你可以按需配置会话的有效期：

```bash
# 空闲超时时间（秒，默认 86400，即 24 小时）
# 每次用户发起请求或交互都会刷新该计时器。
holon config set auth.session.idle_ttl_seconds 43200

# 可选的绝对超时时间（秒，必须大于或等于 idle_ttl_seconds）
# 达到该时长后会话强制失效，无论中途是否活跃。
holon config set auth.session.absolute_ttl_seconds 604800
```

如果不需要绝对超时上限、希望活跃用户一直保持登录状态，可以省略 `auth.session.absolute_ttl_seconds` 或将其设为 `null`。

## 第五步：重启 daemon 生效

认证模式变更需要重启 daemon 后生效：

```bash
holon daemon restart
```

如果是前台运行调试：

```bash
holon serve --access tunnel
```

## 验证与使用

### 1. 从 Web GUI 登录

1. 在浏览器中打开 `https://<your-holon-host>/login`。
2. 登录页面检测到 OIDC 模式后，会显示 **Continue with organization login** 链接。
3. 点击该链接，浏览器将跳转至你的身份提供商登录页。
4. 登录完成后，IdP 会重定向回 `/api/auth/oidc/callback`。Holon 将设置安全的 `holon_session` Cookie 并自动跳到控制面板首页 (`/`)。

### 2. 检查会话身份

通过 Session 检查接口确认当前身份：

```bash
curl -b "holon_session=<session-cookie>" https://<your-holon-host>/api/auth/session/me
```

或者将 Session 凭据放在请求头中：

```bash
curl -H "Authorization: Bearer <session-token>" https://<your-holon-host>/api/auth/session/me
```

接口会返回当前已认证用户的身份与认证方式：

```json
{
  "ok": true,
  "user_id": "oidc-550e8400-e29b-41d4-a716-446655440000",
  "display_name": "Alice Chen",
  "auth_method": "oidc"
}
```

### 3. 查看消息归属（Message Attribution）

在 OIDC 模式下，用户通过 Web GUI 或控制面 API 发送的每一条 Prompt，都会在消息 origin 中记录发送者身份：

- `actor_id`：用户在 Holon 中的稳定标识符（格式为 `oidc-<uuid-v4>`）。
- `actor_display_name`：发送时刻用户的显示名称（若 IdP 未提供姓名 claim 则回退为 `actor_id`）。

无论用户后续在 IdP 中是否修改昵称，历史消息都完整保留发送时刻的快照。审计任务记录或调用 `GET /api/agents/{agent_id}/messages/{message_id}` 时，可以清晰核对每个操作的发起人。

### 4. 退出登录

点击 Web GUI 界面上的**退出登录**，或调用登出接口：

```bash
curl -X POST -H "Authorization: Bearer <session-token>" \
  https://<your-holon-host>/api/auth/session/logout
```

该请求会销毁服务端的会话记录，并清理浏览器中的会话 Cookie。

## 常见问题排查

- **Redirect URI 不匹配**：IdP 中配置的 Callback URL 必须与 `auth.oidc.redirect_uri` 完全一致，包括协议（`https://`）、端口以及路径后缀（`/api/auth/oidc/callback`）。
- **Issuer URL 无法解析或证书无效**：Issuer URL 必须能在公网或内网正常解析，且在生产环境下必须使用受信任的 HTTPS 证书。
- **未读取到 Client Secret**：检查 `auth.oidc.client_secret_env` 指定的环境变量是否在启动 daemon 的当前 shell 或服务环境中正确导出。
- **TTL 配置校验失败**：如果设置了 `auth.session.absolute_ttl_seconds`，其数值必须大于或等于 `auth.session.idle_ttl_seconds`，否则启动时配置校验会报错。

## 相关文档

- [HTTP 控制面参考](/zh-CN/reference/http-control-plane.md) — 会话交换、状态查询与登出端点详情
- [配置参考](/zh-CN/reference/configuration.md) — 完整的 `auth.*` 配置项列表
- [连接远程 Holon 运行时](/zh-CN/guides/connect-remote-runtime.md) — 远程访问模式与网络接入
- [使用 Web GUI](/zh-CN/guides/use-web-gui.md) — 在浏览器中管理 Agent 与任务
