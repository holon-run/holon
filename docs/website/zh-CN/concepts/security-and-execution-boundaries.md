---
title: 安全与执行边界
summary: "Holon 沙箱化与不沙箱化的部分：本地执行、工作区绑定、远程访问、能力密钥和信任元数据。"
order: 30
---

# 安全与执行边界

Holon 运行的 Agent 可以执行 shell 命令、委派工作、等待外部事件。理解 Holon
提供了哪些边界——以及没提供哪些——有助于安全地运行 Agent。

## 宿主本地运行时

Holon 作为用户级进程跑在你的机器上。它**不是沙箱**：

- Agent 可以运行你的用户账号能运行的任何命令。
- 文件操作使用你的用户权限。
- 网络访问遵循宿主机的网络配置。
- 运行时内没有容器、虚拟机或 seccomp 隔离。

**这意味着：** 有意识地限定工作区范围；运行不可信 Agent 工作负载时使用专用
用户账号或容器；对待 Agent 的命令执行，要像对待以你的用户身份运行的 shell
脚本一样谨慎。

## 执行环境摘要

每个 Agent 都会在上下文中收到一份执行环境摘要。这份摘要描述当前策略快照以及
哪些约束是被强制执行的：

| 边界 | 级别 | 含义 |
|----------|-------|---------------|
| `cwd_rooting` | runtime_shaped | 显式请求时，运行时允许 workdir 位于工作区根之外；沙箱是执行后端自己的责任 |
| `projection_rooting` | hard_enforced | 文件系统视图被限制在投影根内 |
| `path_confinement` | not_enforced | 运行时不阻止通过路径访问工作区之外 |
| `write_confinement` | not_enforced | 写操作不限制在工作区内 |
| `network_confinement` | not_enforced | 默认不限制网络访问 |
| `secret_isolation` | not_enforced | 运行时不会把密钥与 Agent 命令隔离 |
| `child_process_containment` | not_enforced | 派生的子进程不受运行时约束 |

**关键结论：** 标为 `hard_enforced` 的边界是运行时的保证。标为
`not_enforced` 的边界依赖操作者的宿主配置（文件系统权限、防火墙规则、
容器隔离）。

## 工作区绑定

Holon 提供工作区绑定来约束 Agent 的活动范围：

- **工作区根** — Agent 的默认工作目录。运行时默认让命令在这个根内启动，但
  执行后端允许时，显式 workdir 可以指向工作区之外。
- **投影根** — 约束呈现给 Agent 的文件系统视图。强制生效时，Agent 只能看到
  投影根下的文件。
- **ApplyPatch 目标** — 文件变更工具默认以活动工作区解析相对路径。

这些绑定是**组织性的护栏**，不是安全边界：能执行任意 shell 命令的 Agent 可以
`cd /etc`，或读取工作区之外的文件——只要你的操作系统权限允许。想要真正的
隔离，就把 Holon 和容器、虚拟机或专用用户账号搭配使用。

## 远程访问（`holon serve`）

`holon serve` 通过 HTTP 暴露 Holon 控制平面。这是很强的能力面，必须保护：

```bash
# 安全：仅本地访问，走 Unix socket（默认守护进程模式）——除文件系统
# 权限外不需要额外保护
holon daemon start

# 谨慎：局域网可达——始终要求 token
holon serve --access lan --token "your-secret-token"
holon serve --access lan --token-file ~/.holon/remote.token

# 谨慎：隧道访问——隧道提供方可以把流量路由到你的 Holon 实例
holon serve --access tunnel --token "your-secret-token"
```

**远程访问规则：**

- 把 Holon 暴露到 localhost 之外时，始终使用 `--token` 或 `--token-file`。
- 除非有明确的集成需求，优先用 `--access local` 或默认守护进程 Unix socket。
- 把 token 当作凭据：轮换它，不要提交它，不要通过未加密渠道分享。
- 隧道模式通过隧道提供方暴露 Holon；使用前先了解提供方的安全模型。

## 能力密钥 URL

Holon 为外部触发和唤醒事件生成回调 URL。这些 URL 包含能力密钥：

```
http://host:7878/callbacks/wake/cb_<secret>
```

知道这个 URL 的任何人都能唤醒 Agent。把这些 URL 当作密钥：

- 不要记录日志、提交或公开发布它们。
- 轮换其他凭据时一并轮换它们。
- 生产部署中使用 HTTPS 和带 token 保护的 serve。

## 信任与来源

Holon 按**来源（origin）**和**信任级别**对每个输入分类：

| 信任级别 | 示例来源 | 含义 |
|-------------|----------------|---------------|
| `trusted-operator` | CLI、TUI、已认证 HTTP | 这个输入由你发起 |
| `trusted-system` | 运行时内部事件、计划定时器 | 由运行时生成 |
| `trusted-integration` | 来自已知服务的已认证 webhook | 由可信外部系统发送 |
| `untrusted-external` | 公开 webhook、用户提交内容 | 来源未知或未验证 |

**信任元数据是策略信号，不是安全保证。** Agent 能看到信任级别并调整行为（例如
对不可信输入拒绝破坏性命令），但信任标签不能替代沙箱。标为
`untrusted-external` 的外部内容仍然以你的用户权限运行，除非你加上操作系统级
隔离。

## 实用建议

1. **使用专用工作区** — 给 Agent 一个具体的项目目录，而不是你的家目录。
2. **保护远程访问** — 始终使用 token，优先本地 Unix socket。
3. **看管能力 URL** — 把唤醒/回调 URL 当作凭据。
4. **不要只依赖信任标签** — 它们影响 Agent 行为，不是操作系统安全。
5. **高风险工作加操作系统级隔离** — 容器、虚拟机或专用用户账号。

## 另见

- [信任边界](/zh-CN/concepts/trust-boundaries) — 信任分类端到端如何工作
- [运行时模型](/zh-CN/concepts/runtime-model) — 执行环境与工作区绑定
- [集成指南](/zh-CN/guides/integration) — HTTP 控制平面访问
- [远程访问](/zh-CN/guides/remote-access) — 在 localhost 之外提供 Holon 服务
