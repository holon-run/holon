---
title: Workspace 与执行环境
summary: Workspace、执行根目录与 Worktree 的绑定、切换及隔离契约。
order: 30
---

# Workspace 与执行环境

有关心智模型，请参阅 [Workspace](/zh-CN/concepts/workspaces.md)；有关操作步骤，请参阅[在 Workspace 或 Worktree 中工作](/zh-CN/guides/work-with-workspaces.md)。
内部不变式与详细契约请查阅[工作区与执行规格](/zh-CN/spec/workspace-and-execution.md)。
每个 Agent 始终只有一个活动 Workspace，它可以指向规范仓库根目录，也可以指向已注册的 Worktree 投影。

## Workspace 定义了什么

| 关注点 | 由 Workspace 设定 |
|---------|-----------------|
| 指令根目录 | `AGENTS.md` 与策略文件的解析基准路径 |
| 执行根目录 | 活动投影中命令的默认工作目录 |
| ApplyPatch 目标 | 相对路径补丁的基准位置；显式绝对路径仍保持绝对目标 |
| 记忆范围 | Workspace 作用域内的片段记录与索引 |

## Workspace 与 Shell 目录的区别

作用域层面的具体区别请参阅 [Workspace](/zh-CN/concepts/workspaces.md)。

| 操作 | 效果 |
|--------|--------|
| 在 shell 里 `cd /other` | 仅影响当前这一个命令进程 |
| `AttachWorkspace` | 添加 Workspace 绑定，但不切换活动投影 |
| `SwitchWorkspace` | 切换后续操作的活动 Workspace |
| `holon workspace attach /path` | 绑定路径，但不改变活动 Workspace |

## Workspace 命令

### 附加（attach）

`holon workspace attach [--agent <id>] <path>`

此命令为目标目录发现或创建 Workspace 记录，并将其绑定至 Agent。附加操作本身不会改变活动 Workspace；准备在该目录工作时，需使用 `SwitchWorkspace` 激活。该绑定跨会话持久保留。

### 退出（exit）

`holon workspace exit [--agent <id>]` 返回 Agent 的 Home Workspace。

### 分离（detach）

`holon workspace detach [--agent <id>] <workspace-id>` 移除目标 Agent 的绑定。如果目标当前处于活动状态，会自动先返回 Agent Home。

分离操作不会删除项目源码文件或保留的 Worktree 产物。与该 Workspace 关联的记忆和片段记录也会完整保留。

## Worktree 隔离机制

`CreateWorktree` 通过显式指定的已附加 Workspace、分支与基准引用创建受管链接 Worktree：

| 字段 | 契约说明 |
|-------|----------|
| `workspace_id` | 必填，已附加的仓库 Workspace |
| `branch` | 必填，检出的目标分支 |
| `base_ref` | 必填，新 Worktree 的起始基准引用 |
| `activate` | 默认为 `true`，创建后立即激活 |
| `on_existing` | `reuse` 或 `error`；`reuse` 要求存在唯一且明确的活动 Worktree，且不会对其执行 reset |
| `label` | 可选，语义化描述标签 |

`RemoveWorktree` 仅接受 `execution_root_id` 或 `path` 中的一个作为唯一定位参数。
若目标 Worktree 处于脏状态、已锁定、未注册、属于规范根目录或被其他 Agent/任务占用，移除将被拒绝。
移除当前活动根目录时必须提供 `return_to`。`branch_policy` 可设为 `keep` 或 `delete_if_merged`；选择后者时可通过 `merged_into` 指定用于比对的合并分支引用。
安全清理流程请参阅 [Workspace 操作指南](/zh-CN/guides/work-with-workspaces.md)。

## Agent Home 与项目 Workspace

角色定位请参阅 [Workspace](/zh-CN/concepts/workspaces.md)，指引文件的配置位置请参阅[定制 Agent](/zh-CN/guides/customize-agent.md)。Agent Home 通常位于 `~/.holon/agents/<agent-id>/`；当配置了自定义 `HOLON_HOME` 或处于特定活动投影时，请以运行时实际报告的路径为准。

## Agent Workspace 工具

- `GetWorkspaceState({})` — 查看绑定、活动投影、Worktree 及占用情况
- `AttachWorkspace({ path: "/repo" })` — 仅附加 Workspace 绑定，不执行切换
- `SwitchWorkspace({ workspace_id: "ws_..." })` — 激活规范仓库根目录
- `SwitchWorkspace({ execution_root_id: "..." })` — 激活保留的 Worktree 投影
- `SwitchWorkspace({ workspace_id: "agent_home" })` — 返回 Agent Home
- `CreateWorktree(...)` — 创建或安全复用链接 Worktree
- `RemoveWorktree(...)` — 仅限清理目的的注册表清理
- `DetachWorkspace({ workspace_id: "ws_..." })` — 移除绑定；活动目标会先返回 Agent Home

`UseWorkspace` 仅作为兼容历史调用的隐藏别名保留。
`SwitchWorkspace` 接受 `workspace_id`、`execution_root_id` 或 `path` 中的唯一选择器，并支持可选的 `cwd`。该工具仅用于激活已有绑定或已注册投影，不会执行仓库附加。Shell 命令中的 `workdir` 和 `cd` 不会改变该激活状态。

## 另见

- [运行时模型](/zh-CN/concepts/runtime-model.md) — 运行时中的 Workspace 生命周期
- [CLI 参考](/zh-CN/reference/cli.md) — 全部 Workspace 相关命令
- [Agent 模板](/zh-CN/reference/agent-templates.md) — 模板如何初始化 Agent Home
