---
title: Workspace 与执行环境
summary: workspace、执行根和 worktree 的绑定、切换与隔离契约。
order: 30
---

# Workspace 与执行环境

Workspace 是 Agent 的执行根目录，Agent 在这里读文件、跑命令、应用补丁。每个
Agent 始终只有一个活动 workspace。

## Workspace 定义了什么

| 关注点 | 由 workspace 决定 |
|---------|-----------------|
| 指令根目录 | `AGENTS.md` 和政策文件从哪里解析 |
| 执行根目录 | 命令的默认工作目录 |
| ApplyPatch 目标 | 文件改动落在哪里 |
| 记忆范围 | Workspace 范围内的片段记录和索引 |

## Workspace 与 shell 目录的区别

Workspace **不是** shell 的 `cd`。Shell `cd` 只改变那一个命令进程的目录。
Agent 用显式的绑定和激活工具来改变运行时工具的操作位置。

| 操作 | 效果 |
|--------|--------|
| 在 shell 里 `cd /other` | 只影响该命令 |
| `AttachWorkspace` | 添加 workspace 绑定，但不切换 |
| `SwitchWorkspace` | 改变后续操作的活动 workspace |
| `holon workspace attach /path` | 为路径附加绑定，但不切换活动 workspace |

## Workspace 命令

### 附加（attach）

把项目目录附加到 Agent：

```bash
holon workspace attach /path/to/project
holon workspace attach --agent my-agent /path/to/project
```

这会为该目录发现或创建 workspace 记录，并把它绑定到 Agent。附加本身不会改变
活动 workspace；准备在那里工作时，用 `SwitchWorkspace` 激活它。该绑定会跨
会话保留。

### 退出（exit）

返回 Agent 的 home workspace：

```bash
holon workspace exit
holon workspace exit --agent my-agent
```

### 分离（detach）

彻底移除一条 workspace 记录：

```bash
holon workspace detach <workspace-id>
```

分离不会删除目录，只是把 workspace 记录从 Holon 的索引中移除。与该 workspace
关联的记忆和片段记录会保留。

## Worktree 隔离

`CreateWorktree` 依据一个显式附加的 workspace、分支和基准引用，创建一个受管理的
链接 worktree：

```text
CreateWorktree {
  workspace_id: "ws_...",
  branch: "feature/example",
  base_ref: "origin/main"
}
```

隔离 workspace 适合：

- 安全试验，不污染工作副本
- 不同 Agent 在同一仓库上并行工作
- PR 评审分支，改动不应外泄

## Agent Home 与项目 Workspace

| Workspace 类型 | 用途 | 示例 |
|---------------|---------|---------|
| Agent home | Agent 本地状态和记忆 | `~/.holon/agents/my-agent/` |
| 项目 workspace | 正在处理的代码和文件 | `/path/to/project` |

每个 Agent 启动时都以自己的 Agent home 作为活动 workspace。用
`workspace attach` 绑定项目 workspace，用 `SwitchWorkspace` 激活它，用
`workspace exit` 返回 Agent home。

## Agent Workspace 工具

- `GetWorkspaceState({})` — 查看绑定、活动投影、worktree 和占用情况
- `AttachWorkspace({ path: "/repo" })` — 附加但不切换
- `SwitchWorkspace({ workspace_id: "ws_..." })` — 激活规范根目录
- `SwitchWorkspace({ execution_root_id: "..." })` — 激活一个保留的 worktree
- `SwitchWorkspace({ workspace_id: "agent_home" })` — 返回 Agent home
- `CreateWorktree(...)` — 创建或安全复用链接 worktree
- `RemoveWorktree(...)` — 仅限清理的注册表清理
- `DetachWorkspace({ workspace_id: "ws_..." })` — 移除绑定；活动目标会先返回 Agent home

`UseWorkspace` 仍是为历史调用保留的隐藏兼容别名。

## 另见

- [运行时模型](/zh-CN/concepts/runtime-model.md) — 运行时中的 workspace 生命周期
- [CLI 参考](/zh-CN/reference/cli.md) — 全部 workspace 命令
- [Agent 模板](/zh-CN/reference/agent-templates.md) — 模板如何初始化 Agent home
