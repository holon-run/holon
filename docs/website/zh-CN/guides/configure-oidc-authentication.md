---
title: 配置 OIDC 认证
summary: 配置 OpenID Connect 单点登录、团队共享运行时提示词审计与会话超时策略。
order: 180
---

# 配置 OIDC 认证

默认情况下，Holon 使用本地控制 Token 进行身份认证。对于团队共享服务器或远程多用户部署，可以切换为 OpenID Connect (OIDC) 单点登录。团队成员通过企业现有的身份提供商（IdP）登录，Holon 会精准记录向 Agent 发起提示词的具体用户身份。

本指南涵盖在 IdP 中注册 OIDC 客户端、配置 Holon、调整会话生命周期、多成员共享任务核验以及登录流程验证。

## 前置条件

- 运行中且用户可访问的 Holon 实例（例如通过 `--access tunnel`、`--access tailnet` 或反向代理对外提供服务）。
- 兼容 OIDC 规范的身份提供商，例如 Keycloak、Okta、Authentik、Google Workspace 或 Microsoft Entra ID。
- 在 IdP 中具有注册新客户端应用的管理员权限。

> **安全注意：** Holon 要求 OIDC 发行者 URL（issuer URL）与回调端点必须使用 HTTPS。仅当主机名严格为 `localhost` 时，回调地址才允许使用 HTTP；发行者 URL 必须始终使用 HTTPS。

修改认证配置时请保持对运行时主机的运维通道畅通，以便在配置出错时及时修正。OIDC 仅用于识别登录用户身份；它不会建立底层沙箱，也不代表实现了用户间的文件系统或工作区隔离。具体请参阅[安全与执行边界](../concepts/security-and-execution-boundaries.md)。

## 第 1 步：在 IdP 中注册 Holon 客户端

在身份提供商中创建新的 OpenID Connect 应用：

1. **Client ID**：拟定客户端标识，例如 `holon`。
2. **Client 认证方式**：针对授权码（Authorization Code）登录流程注册机密客户端（Confidential Client），并生成 **Client Secret**。这是面向用户登录的流程，并非 Client Credentials 流程。
3. **重定向 URI（Redirect URI）**：配置登录回调地址：
   ```text
   https://<your-holon-host>/api/auth/oidc/callback
   ```
   若在本地 `7878` 端口测试，可配置为：
   ```text
   http://localhost:7878/api/auth/oidc/callback
   ```
4. **权限范围（Scopes）**：确保客户端至少申请了 `openid`、`profile` 与 `email` 权限。

妥善记录下 **Issuer URL**、**Client ID** 以及 **Client Secret**。

## 第 2 步：将 Client Secret 存入环境变量

避免将 Secret 明文写入 Holon 配置文件或提交至版本控制。在守护进程运行的主机环境中通过环境变量注入：

```bash
export HOLON_OIDC_CLIENT_SECRET="your-oidc-client-secret"
```

如果使用 systemd 服务或容器运行 Holon，请通过服务配置文件的密钥注入或环境配置注入该变量。严格限制对底层环境配置文件的读取权限。在互不相关的其他 Shell 会话中 export 不会影响已运行的服务进程。

## 第 3 步：配置 Holon

使用 `holon config set` 设置认证模式与服务商参数：

```bash
# 切换到 OIDC 认证模式
holon config set auth.mode "oidc"

# 设置发行者 URL（必须支持在 /.well-known/openid-configuration 进行 OIDC 发现）
holon config set auth.oidc.issuer_url "https://auth.example.com/realms/team"

# 设置已注册的 Client ID
holon config set auth.oidc.client_id "holon"

# 指向包含 Secret 的环境变量名
holon config set auth.oidc.client_secret_env "HOLON_OIDC_CLIENT_SECRET"

# 设置公网可访问的回调 URL（在反向代理环境下建议显式配置）
holon config set auth.oidc.redirect_uri "https://holon.example.com/api/auth/oidc/callback"
```

## 第 4 步：配置文件会话策略

Holon 会为浏览器下发 HttpOnly 会话 Cookie，为 API 客户端下发会话凭据。你可以按需配置会话的有效时长：

```bash
# 空闲会话有效时长，单位秒（默认：604800，即 7 天）
# 期间发生的合法交互操作会自动刷新该计时器。
holon config set auth.session.idle_ttl_seconds 43200

# 可选的绝对会话超时时间，单位秒（必须 >= idle_ttl_seconds）
# 配置后，无论用户是否处于活跃状态，超过此时长后必须重新登录。
holon config set auth.session.absolute_ttl_seconds 604800
```

若希望禁用绝对超时，允许活跃用户持续保持登录状态，可省略 `auth.session.absolute_ttl_seconds` 或将其置为 `null`。

## 第 5 步：重启守护进程

重启守护进程使认证变更生效：

```bash
holon daemon restart
```

如果是在前台交互式运行 Holon：

```bash
holon serve --access local
```

保持使用与此前一致的安全网络端点或反向代理。如果同时变更了公网主机名，务必同步更新 IdP 中注册的重定向 URI。直接启动非回环接口、LAN 或 tailnet 命令行监听时，启动瞬间依然要求配置控制 Token；开启 OIDC 登录不会移除该底层的监听检查。

## 验证配置

### 1. 通过 Web GUI 登录

1. 在浏览器中打开 `https://<your-holon-host>/login`。
2. 登录页面识别到 OIDC 模式，展示 **Continue with organization login** 入口。
3. 点击链接跳转至你的身份提供商登录页。
4. 完成身份登录。IdP 会重定向回 `/api/auth/oidc/callback`，服务端写入安全的 `holon_session` Cookie 并跳转进入仪表盘主页（`/`）。

### 2. 验证会话身份

通过会话接口检查当前登录身份：

```bash
curl -b "holon_session=<session-cookie>" https://<your-holon-host>/api/auth/session/me
```

或将 Session Token 作为 Bearer 凭据传入：

```bash
curl -H "Authorization: Bearer <session-token>" https://<your-holon-host>/api/auth/session/me
```

端点会返回认证后的用户身份与鉴权方式：

```json
{
  "ok": true,
  "user_id": "oidc-550e8400-e29b-41d4-a716-446655440000",
  "display_name": "Alice Chen",
  "auth_method": "oidc"
}
```

### 3. 检查消息归属审计

在 OIDC 模式下，通过控制平面发送的每条用户提示词都会在消息来源（origin）中记录发送者的身份：

- `actor_id`：用户在 Holon 中的持久标识（格式为 `oidc-<uuid-v4>`）。
- `actor_display_name`：用户在发送消息时的显示名称（若缺少 name 声明则回退为 `actor_id`）。

在审计 Agent 运行轨迹或通过 `GET /api/agents/{agent_id}/messages/{message_id}` 查询消息时，可以明确追溯是谁下发的指令。该归属信息并不代表该用户逐一手动批准了后续触发的每一次具体工具调用。

### 4. 用两名测试成员核对同一项共享任务

在团队多人共用同一套 Agent 运行时的部署场景下，建议核对独立会话发起的任务归属，以及成员协同查看共享任务结果的流程：

1. **前置测试范围约定**：在执行测试前明确测试边界：

   | 约定项 | 测试配置要求 |
   | --- | --- |
   | **测试账号** | 两名已配置的测试用户（例如 `alice@example.com` 与 `bob@example.com`） |
   | **目标 Agent** | 约定的团队共享 Agent（例如 `team-assistant`） |
   | **允许执行的任务** | 有界的只读检查任务（例如查询某次构建状态或报告） |
   | **预期共享范围** | 两位成员均可查阅该任务结果；每条提示词精准记录对应的发送者 `actor_id` |
   | **非预期范围** | 用户间底层文件系统沙箱或隔离工作区（OIDC 不提供此类隔离） |

2. **成员 A 发起任务**：
   - 成员 Alice 在独立浏览器会话 A 中登录，向共享 Agent 发送指令：“检查提交 6a21e3c4 的构建状态”。
   - Alice 获得任务或工作项 ID。
   - 查看该消息详情，确认 `actor_display_name` 记录为 Alice，且携带其专属 `actor_id`。

3. **成员 B 跟进共享结果**：
   - 在另一个独立的浏览器窗口或无痕会话中，成员 Bob 登录。
   - Bob 进入该共享 Agent，找到 Alice 发起的任务结果，并提交跟进提示词：“总结刚才检查中的验证项”。
   - 消息轨迹中清晰记录 Bob 的独立 `actor_id` 与显示名称，同时保持了该工作项任务上下文的连贯性。

4. **核对审计轨迹**：
   - 通过 `GET /api/agents/{agent_id}/messages` 查看消息列表：
   - 两条提示词按时间顺序记录，各自绑定真实发送者的 OIDC 身份。

> **安全与隔离边界说明：**
> - **认证、授权与底层执行的三重边界：** OIDC 负责解决“发起者是谁”的身份认证与审计归因。它**不提供**多租户文件系统虚拟化、容器安全边界或每用户独立工作区隔离。
> - **底层执行环境：** Agent 调用的工具均运行在主机守护进程的用户权限下。如需对不可信任务进行硬隔离，应部署独立的守护进程实例或容器环境。具体请参阅[安全与执行边界](../concepts/security-and-execution-boundaries.md)以及[小团队中的 Agent 实践](../blog/agents-in-a-small-team.md)。
> - **实测先决条件：** 完整的双用户端到端测试需要获授权的非生产测试实例以及两个测试身份。在尚未具备多用户测试凭据时，核对单用户归因即可，并将双用户任务测试标记为等待环境授权，切勿虚构测试记录。

### 5. 退出登录

点击 Web GUI 中的 **Log out**，或调用登出接口：

```bash
curl -X POST -H "Authorization: Bearer <session-token>"   https://<your-holon-host>/api/auth/session/logout
```

登出会注销服务端对应的会话记录。在浏览器中登出会同步清除本地 Cookie；单独执行 `curl` 请求不会清理你浏览器中的 Cookie。

### 6. 验证未经授权请求的拦截表现

发起不带 Cookie 和 Bearer 凭据的请求：

```bash
curl -i https://<your-holon-host>/api/auth/session/me
```

预期应返回未鉴权错误，而不是用户信息。使用登出后已吊销的凭据重复该请求，确认其同样被拦截拒绝。验证登录流程时，建议使用浏览器无痕隐私窗口，避免受既有会话干扰。

## 故障排查

- **重定向 URI 不匹配（Redirect URI mismatch）：** 确认 IdP 中的回调地址与 `auth.oidc.redirect_uri` 完全一致（逐字节匹配协议 `https://`、端口号及结尾路径 `/api/auth/oidc/callback`）。
- **无效的发行者 URL（Invalid issuer URL）：** 发行者 URL 必须与 ID Token 中的 `iss` 声明完全一致，且能通过 HTTPS 访问其 `/.well-known/openid-configuration`。
- **未找到 Secret：** 检查 `auth.oidc.client_secret_env` 中声明的环境变量是否已成功注入守护进程进程树。
- **TTL 配置不合法：** 若配置了 `auth.session.absolute_ttl_seconds`，其数值必须大于或等于 `auth.session.idle_ttl_seconds`。否则 Holon 在启动校验时会直接报错退出。
- **登录成功但身份不匹配：** 在同一会话中请求 `/api/auth/session/me`，核实 IdP 实际登录的账户。切勿单凭显示名称推断身份归属。
- **登出后刷新网页立即自动重新登录：** 检查浏览器在 IdP 端是否仍保持着活跃的登录态。Holon 会话登出并不等同于 IdP 全局企业单点登出。

## 另见

- [HTTP 控制平面参考](/zh-CN/reference/http-control-plane.md) — 会话兑换、状态查询与登出接口定义
- [配置参考](/zh-CN/reference/configuration.md) — `auth.*` 配置项完整清单
- [连接到远程 Holon 运行时](/zh-CN/guides/connect-remote-runtime.md) — 远程访问方案与网络拓扑
- [使用 Web GUI](/zh-CN/guides/use-web-gui.md) — 在浏览器中操作 Agent 与任务
- [安全与执行边界](../concepts/security-and-execution-boundaries.md) — 认证、授权与隔离的三重边界
- [小团队中的 Agent 实践](../blog/agents-in-a-small-team.md) — 团队共享 Agent 协作与审计实践
