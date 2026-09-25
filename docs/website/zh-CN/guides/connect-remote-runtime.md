---
title: 连接远程 Holon 运行时
summary: 连接另一台机器上的运行时并验证连接可用。
order: 15
---

# 连接远程 Holon 运行时

在一台机器上运行 Holon，从另一台机器使用它——团队共享服务器、无头主机，或远程开发机。本页帮你选访问方式、建立连接，并确认连接可用。

## 访问模式

`holon daemon start` 和 `holon serve` 都接受 `--access` 标志：

| 模式 | 说明 | 适用场景 |
|------|------|----------|
| `local` | 仅回环地址（127.0.0.1） | 默认；单机使用 |
| `lan` | 本地网络 | 同一局域网、已知 IP |
| `tunnel` | Cloudflare Tunnel | 通过隧道公网访问 |
| `tailnet` | Tailscale 网络 | 自己设备之间的私有 mesh |

## 远程服务端

### Tunnel 模式（Cloudflare）

启动一个可通过 Cloudflare Tunnel 访问的 daemon：

```bash
holon daemon start --access tunnel
```

也可以使用独立的服务端：

```bash
holon serve --access tunnel
```

隧道生命周期由运行时管理。你这边不需要任何 Cloudflare 配置——Holon 会自动创建并管理临时隧道。

### Tailnet 模式（Tailscale）

在自己设备之间做私有 mesh 访问：

```bash
holon daemon start --access tailnet
holon serve --access tailnet
```

需要宿主机已安装并认证 Tailscale。

### LAN 模式

同网络、已知 IP 时使用：

```bash
holon serve --access lan --host 192.168.1.10 --port 8787
```

### 自定义主机和端口

覆盖默认监听地址：

```bash
holon daemon start --access tunnel --port 9000
holon serve --access lan --host 0.0.0.0 --port 8787
```

## 远程连接

### TUI 连接

从远程终端连接：

```bash
holon tui --connect https://your-server:8787 --token "your-token"
```

从文件读取 token：

```bash
holon tui --connect https://your-server:8787 --token-file ~/.holon/remote.token
```

使用已保存的 token profile：

```bash
holon tui --connect https://your-server:8787 --token-profile my-profile
```

### HTTP API

同一个 token 也用于认证 HTTP 控制平面请求：

```bash
curl -H "Authorization: Bearer your-token" \
  https://your-server:8787/api/agents/list
```

完整 API 面见 [HTTP 控制平面参考](/zh-CN/reference/http-control-plane.md)。

## Token 管理

### 提供 token

Holon 不会替你生成控制 token。自己选一个密钥，通过 `--token`、`--token-file` 或 `HOLON_CONTROL_TOKEN` 环境变量交给服务端：

```bash
# 从文件读取控制 token
holon daemon start --access tunnel --token-file ~/.holon/remote.token
```

### Token profile

把多个 token 存成命名的凭据 profile，再按名字选用：

```bash
holon config credentials set office --kind bearer_token --stdin
holon config credentials set home --kind bearer_token --stdin
```

然后按 profile 名连接：

```bash
holon tui --connect https://office:8787 --token-profile office
holon tui --connect https://home:8787 --token-profile home
```

### 通过 Android 客户端进行移动端访问

你也可以使用原生的 Holon Android 客户端在手机或平板上连接远程运行时。输入远程 HTTPS 基础 URL 和访问凭据即可建立经过系统 Keystore 加密的移动端会话。详见[连接 Android 客户端](/zh-CN/guides/connect-android-client.md)。


## Daemon 管理

daemon 启动后，常规管理命令都能远程使用：

```bash
holon daemon status
holon daemon logs
holon daemon restart
holon daemon stop
```

## 安全注意事项

- **始终使用 token**。只要监听非回环地址，或使用 `--access lan`/`--access tailnet`，Holon 就会拒绝在没有 token 的情况下启动。`--access tunnel` 也应设置 token：隧道是公网可达的。
- **跨公网连接时优先用 tunnel 或 tailnet**，而不是 LAN 模式。它们提供加密和认证，无需暴露裸端口。
- **轮换 token**：用新的 `--token-file` 重启 daemon。
- **只需要本机连接时用 `--access local`**。这是默认值，也是最安全的选择。
- HTTP 控制平面会执行信任边界规则：即使是只读路由（Agent 状态、事件、任务），远程访问也仍然需要有效 token。
- **团队部署与多用户**：在多成员协作环境中，建议配置 OIDC 身份认证（`auth.mode = "oidc"`），使用企业单点登录取代单一共享 Token，并审计各操作人身份。参见[配置 OIDC 身份认证](/zh-CN/guides/configure-oidc-authentication.md)。

## 另请参阅

- [TUI 参考](/zh-CN/reference/tui.md) — 导航、斜杠命令和远程连接
- [HTTP 控制平面](/zh-CN/reference/http-control-plane.md) — 编程访问的 API 参考
- [排查 Holon 任务问题](/zh-CN/guides/troubleshooting.md) — 连接问题
- [通过 HTTP 自动化 Holon](/zh-CN/guides/automate-over-http.md) — 用代码驱动 Holon
- [配置 OIDC 身份认证](/zh-CN/guides/configure-oidc-authentication.md) — 单点登录与审计归属
- [连接 Android 客户端](/zh-CN/guides/connect-android-client.md) — 在移动设备上管理 Agent
