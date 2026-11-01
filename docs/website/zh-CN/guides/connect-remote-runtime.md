---
title: 连接到远程 Holon 运行时
summary: 从另一台电脑或手机访问远程运行时，验证连接并查回已有任务结果。
order: 160
---

# 连接到远程 Holon 运行时

将 Holon 部署在服务器或远程主机上，并从笔记本电脑、台式机或手机发起连接。无论是需要在移动设备上随时查验个人助手的任务简报，还是在公用服务器上为全组搭建一个长期待命的 AI 助手（详见[与团队共用 AI 助手](./share-with-your-team.md)），你都可以让运行时持续驻留后台，并通过安全网络连接。

## 前置条件

- 运行时主机已安装 Holon；若使用 TUI 访问，客户端机器上也需安装。
- 具备管理主机网络和重启守护进程的权限。
- 在主机上准备一个仅限运行 Holon 的账户可读的控制 Token 文件。详见[提供 Token](#providing-a-token)。
- 规划客户端可达的网络路径。如果是公网访问，务必配置 HTTPS。

下文中的服务端命令均在运行时主机上执行。管理服务时请在后台守护进程（daemon）或前台 `holon serve` 中二选一，切勿在同一端口同时启动两者。

## 访问模式

`holon daemon start` 与 `holon serve` 均支持 `--access` 参数：

| 模式 | 说明 | 适用场景 |
|---|---|---|
| `local` | 仅回环地址（127.0.0.1） | 默认值，单机开发使用 |
| `lan` | 局域网监听 | 同一局域网内，已知固定 IP |
| `tunnel` | Cloudflare Tunnel | 自动通过安全隧道接入公网 |
| `tailnet` | Tailscale 虚拟网格 | 仅限你名下设备间的私有互联 |

## 启动远程服务

### 隧道模式（Cloudflare Tunnel）

启动通过 Cloudflare Tunnel 暴露的守护进程：

```bash
holon daemon start --access tunnel --token-file ~/.holon/remote.token
```

或使用前台独立服务启动：

```bash
holon serve --access tunnel --token-file ~/.holon/remote.token
```

运行时会自动管理隧道的生命周期，无需你在 Cloudflare 端手动预配置 — Holon 会自动创建并维护临时安全隧道。请以运行时实际输出的公网 URL 为准，不要把本地监听端口当作公网地址。隧道本质上是通过第三方代理暴露控制平面，因此必须始终开启 Holon 身份鉴权。

### Tailnet 模式（Tailscale）

适用于在你的私有设备互联网络中安全访问：

```bash
holon daemon start --access tailnet --token-file ~/.holon/remote.token
# 或在前台运行：
holon serve --access tailnet --token-file ~/.holon/remote.token
```

要求主机已安装并登录认证 Tailscale。客户端也必须加入同一 tailnet 才能触达主机。Tailnet 连通性与 Tailscale Serve 提供的 HTTPS 是两码事；请以实际配置的协议与 URL 为准。

### 局域网模式（LAN）

在同一局域网中直连已知 IP：

```bash
holon serve --access lan --host 192.168.1.10 --port 8787 \
  --token-file ~/.holon/remote.token
```

### 自定义监听地址与端口

覆盖默认的监听配置：

```bash
holon daemon start --access tunnel --port 9000 --token-file ~/.holon/remote.token
holon serve --access lan --host 0.0.0.0 --port 8787 \
  --token-file ~/.holon/remote.token
```

绑定 `0.0.0.0` 会监听所有 IPv4 网络接口（可能包含公网接口）。单纯绑定接口不提供 TLS 加密；请配合主机防火墙收敛暴露面，并在流量离开可信局域网时一律配置 HTTPS。

## 发起远程连接

将下述示例中的地址替换为主机实际通告或配置的基准 URL。对外暴露的 HTTPS 端点即使内部监听在其他端口，外部也通常映射在 443 端口。在浏览器中直接打开该 URL 即可访问 Web GUI。

### TUI 终端连接

从远程终端发起连接：

```bash
holon tui --connect https://your-server:8787 --token "your-token"
```

从文件读取 Token：

```bash
holon tui --connect https://your-server:8787 --token-file ~/.holon/remote.token
```

使用已保存的凭据配置文件：

```bash
holon tui --connect https://your-server:8787 --token-profile my-profile
```

### HTTP API 访问

控制平面的 HTTP 请求使用同样的 Token 进行鉴权：

```bash
curl -H "Authorization: Bearer your-token" \
  https://your-server:8787/api/agents/list
```

完整接口定义请参阅 [HTTP 控制平面参考](/zh-CN/reference/http-control-plane.md)。

## Token 管理

<a id="providing-a-token"></a>

### 提供 Token

直接通过命令行启动时，需提供自定义控制 Token。可以通过 `--token`、`--token-file` 或环境变量 `HOLON_CONTROL_TOKEN` 传入：

```bash
# 从文件读取控制 Token
holon daemon start --access tunnel --token-file ~/.holon/remote.token
```

该文件应由高强度随机生成的机密字符构成。严禁将其提交到代码仓库、写入日志或明文外发。
请通过安全通道将 Token 分发给客户端，或者对支持的客户端使用[设备扫码配对](connect-android-client.md#方案-a二维码扫码配对推荐)。优先使用 Token 文件，避免在命令行参数中直接暴露明文机密。

### 凭据配置文件（Token Profiles）

可以将多个服务端的 Token 存储为命名的凭据配置文件，方便后续按名称调用：

```bash
holon config credentials set office --kind bearer_token --stdin
holon config credentials set home --kind bearer_token --stdin
```

随后按 profile 名称直接连接：

```bash
holon tui --connect https://office:8787 --token-profile office
holon tui --connect https://home:8787 --token-profile home
```

### 移动端访问

你可以使用原生移动客户端从手机或平板连接到远程运行时。通过二维码扫码配对或输入远程 HTTPS URL 与 Token 开启加密会话：
- [使用 Android 客户端连接](/zh-CN/guides/connect-android-client.md) — 安装 APK 或源码构建
- [使用 iOS 客户端连接](/zh-CN/guides/connect-ios-client.md) — 从 iPhone 或 iPad 连接

## 守护进程管理

在运行时主机上执行以下命令（例如通过安全的 SSH 会话）。这些命令只管理当前主机本机的守护进程，不会作用于之前通过 `holon tui --connect` 连接的远端服务：

```bash
holon daemon status
holon daemon logs
holon daemon restart
holon daemon stop
```

## 验证连接

1. 打开远程 Web GUI 或使用 TUI 建立连接，确认显示预期的 Agent 名册。核对基准 URL，避免误连其他环境。
2. 向某个已知 Agent 发送一条安全的测试消息，等待其返回新的回复。页面正常加载或请求被服务端接收，并不代表 Agent 已最终完成了任务。
3. **查回已有任务：** 在远程客户端中导航至已记录的 WorkItem ID，确认能够正确读取服务器端刚刚生成或先前保存的任务完成简报与产物文件。
4. 携带配置的凭据请求 `/api/agents/list`，检查是否成功返回 JSON 响应。
5. 在不携带凭据的情况下重复该请求：

   ```bash
   curl -i https://your-server:8787/api/agents/list
   ```

   正常情况下应返回鉴权失败错误，而非 Agent 列表数据。如果未经鉴权就返回了受保护数据，必须立即停止对外暴露该端点，排查鉴权配置。

## 故障排查

- **连接被拒绝或超时：** 检查主机上守护进程的运行状态、监听地址、端口开放情况、防火墙规则以及客户端的网络通路。
- **Tailnet 主机不可达：** 检查两端设备是否均已连入 Tailscale，且 ACL 规则允许相互访问。
- **鉴权报错：** 核对客户端凭据与服务端的认证模式。对于 OIDC 部署，应使用组织单点登录，不可误把共享控制 Token 当作用户会话。
- **GUI 能打开但 API 报错：** 确保 Web GUI 与 API 部署在同源根路径下，或检查反向代理与 CORS 配置。
- **隧道 URL 失效：** 检查主机上当前通告的隧道 URL 和运行状态；临时隧道 URL 在服务重启后不会保持不变。

## 安全考量

- **务必启用 Token：** 监听非回环地址，或开启 `--access lan`/`--access tailnet` 时，若未设置 Token，Holon 会直接拒绝启动。在 `--access tunnel` 模式下同样必须设置 Token，因为公网隧道处于完全公开可达状态。
- **优先采用隧道或 Tailnet：** 尽量避免将原生局域网监听直接映射暴露到公网上。必须始终开启 Holon 身份鉴权；网络层隔离不能替代应用鉴权。
- **轮换 Token：** 使用新的 `--token-file` 重启守护进程即可轮换 Token。
- **本地使用保持 `--access local`：** 仅在本地操作时保留默认的 local 模式，这是最安全的选项。
- 控制平面严格遵守信任边界：只读接口（Agent 状态、事件、任务等）在远程访问时同样需要有效 Token。
- **多用户团队部署：** 共享的多用户环境应切换为 OIDC 身份认证（`auth.mode = "oidc"`），以独立的 SSO 登录取代单一的共享控制 Token。详见[配置 OIDC 认证](/zh-CN/guides/configure-oidc-authentication.md)。

远程认证仅用于控制进入运行时的权限，它不会对 Agent 命令实施强沙箱隔离，也不会隔离不同用户的文件系统。在包含敏感文件的主机上暴露服务前，请仔细阅读[安全与执行边界](../concepts/security-and-execution-boundaries.md)。

## 另见

- [TUI 参考](/zh-CN/reference/tui.md) — 终端导航、斜杠命令与远程连接
- [HTTP 控制平面](/zh-CN/reference/http-control-plane.md) — 面向程序化调用的完整 API 契约
- [排查 Holon 任务故障](/zh-CN/guides/troubleshooting.md) — 网络连通性问题排查
- [通过 HTTP 自动化执行](/zh-CN/guides/automate-over-http.md) — 使用代码调度与驱动 Holon
- [使用 Android 客户端连接](/zh-CN/guides/connect-android-client.md) — 在移动设备上管理 Agent
- [使用 iOS 客户端连接](/zh-CN/guides/connect-ios-client.md) — 从 iOS 设备查验任务
- [配置 OIDC 认证](/zh-CN/guides/configure-oidc-authentication.md) — 单点登录与审计日志
