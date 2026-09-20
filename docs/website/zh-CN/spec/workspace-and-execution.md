---
title: Workspace 与执行
summary: 当前 workspace 身份、agent home、execution root、worktree 和 host-local 策略契约。
order: 70
---

# Workspace 与执行

本页定义 workspace 身份、execution root、worktree 隔离和 host-local 执行策略的当前契约。

> **Last verified:** 2026-09-13 against `src/types.rs`
> `ActiveWorkspaceEntry`, `WorkspaceOccupancyRecord`, `WorktreeSession`,
> `src/system/types.rs` `ExecutionSnapshot`, and `src/runtime/workspace.rs`.

## 源 RFC

- [Workspace Binding and Execution Roots](https://github.com/holon-run/holon/blob/main/docs/rfcs/workspace-binding-and-execution-roots.md)
- [Workspace Entry and Projection](https://github.com/holon-run/holon/blob/main/docs/rfcs/workspace-entry-and-projection.md)
- [Agent Workspace Tool Surface](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-workspace-tool-surface.md)
- [Execution Root Registry](https://github.com/holon-run/holon/blob/main/docs/rfcs/workspace-execution-root-registry.md)
- [Execution Policy and Virtual Execution Boundary](https://github.com/holon-run/holon/blob/main/docs/rfcs/execution-policy-and-virtual-execution-boundary.md)
- [Agent Home Directory Layout](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-home-directory-layout.md)
- [Instruction Loading](https://github.com/holon-run/holon/blob/main/docs/rfcs/instruction-loading.md)
- [Agent and Workspace Memory](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-and-workspace-memory.md)

## 核心模型

每个 Agent 恰好有一个**活动 workspace（active workspace）**。活动 workspace 定义：

| 概念 | 含义 |
|---------|---------|
| `workspace_id` | workspace 的稳定标识符 |
| `workspace_anchor` | workspace 根目录的文件系统路径 |
| `execution_root` | 进程执行的根（可与 anchor 不同） |
| `cwd` | shell 命令的当前工作目录 |
| `projection_kind` | workspace 的投影方式（`CanonicalRoot`、`GitWorktreeRoot`） |
| `access_mode` | Agent 持有 workspace 的方式（`SharedRead`、`ExclusiveWrite`） |

### 活动 workspace 与 shell `cd`

- 活动 workspace 是**运行时状态**，不是 shell 状态。
- `ExecCommand` 中的 shell `cd` 只改变该条命令的工作目录，不改变活动 workspace、
  指令根、AGENTS.md 作用域或 `ApplyPatch` 的相对路径基准。
- `SwitchWorkspace` 激活已有的 workspace 或 execution root。
- `AttachWorkspace` 添加绑定，但不改变活动投影。

## Agent home

`agent_home` 是 Agent 本地状态的内置回退 workspace：

| 目录 | 用途 |
|-----------|---------|
| `AGENTS.md` | 长期 Agent 契约（作为指引加载） |
| `memory/` | 精选记忆 markdown（`self.md`、`operator.md`） |
| `notes/` | 工作笔记 |
| `work-items/` | WorkItem 计划工件（`plan.md`） |
| `skills/` | Agent 本地 skills |
| `tmp/` | 短生命周期工作文件；随时可能被清理 |
| `.holon/` | 运行时拥有的状态、账本、索引、缓存 |

**关键契约：**

- `.holon/` 由运行时拥有；Agent 不得编辑。
- `AGENTS.md` 可以演进，但应承载持久的 Agent 行为，而不是临时计划或复制的项目文档。
- 即使没有附加项目 workspace，`agent_home` 也始终可作为 workspace 使用。

## 工作区占用

workspace 跟踪**占用（occupancy）**：哪个 Agent 持有它，以及如何持有：

| 字段 | 用途 |
|-------|---------|
| `holder_agent_id` | 当前占用该 workspace 的 Agent |
| `access_mode` | `SharedRead` 或 `ExclusiveWrite` |
| `acquired_at` | 获得占用的时间 |
| `released_at` | 释放占用的时间（如果已释放） |

占用用于协调，不是硬锁。运行时用占用记录做诊断和清理，不在文件系统层面阻止并发访问。

## Worktree

当 Agent 需要隔离的文件改动时，`CreateWorktree` 会基于显式的 `branch` 和
`base_ref` 创建或安全复用运行时管理的链接 worktree：

- worktree 拥有独立于规范 workspace 的 `execution_root`。
- 切走后 worktree 工件仍会保留。
- `RemoveWorktree` 只做干净移除，并可选地按合并可达性删除分支。
- worktree 使用 host-local 文件系统上的 git worktree；不是容器化沙箱。

## 执行快照（`ExecutionSnapshot`）

`AgentSummary` 中的 `ExecutionSnapshot` 记录当前执行上下文：

- 执行 profile 与策略快照（backend、进程执行、后台任务、托管 worktree 标志）
- 已附加的 workspace 与已注册的 execution root
- 活动 workspace id、anchor、execution root、execution root id 与 cwd
- 投影类型与访问模式
- 当 execution root 是 worktree 时，记录 worktree 根

外层 `AgentSummary` 还会报告活动 run id（`agent.current_run_id`）、活动
workspace 占用以及 worktree 会话。

## Host-local 策略

Holon 当前的执行模型是 **host-local**：进程以 Agent 用户的权限在宿主文件系统上运行。
关键约束：

- `cwd` 始终位于 execution root 内。
- 进程执行不由运行时容器化或沙箱化。
- 网络访问默认不受限制。
- 模型上下文中的 `execution_environment` 摘要把当前策略快照描述为透明度契约，
  而不是硬性沙箱保证。

## 文件引用与输出交付契约

Holon 对结果交付与跨输出表面的文件引用制定了明确契约：

### 自包含的输出交付

Agent 交付以简报（brief）为核心。最终简报与助手消息必须自包含：操作者无需翻阅中间工具日志或打开引用的文件，即可完整了解工作结果、验证状态、风险和所需的后续操作。文件引用仅作为辅助产物的入口，不能替代结果摘要本身。

### 按输出表面选择文件引用格式

文件引用的具体表达方式取决于其展示和消费的表面：

- **项目内 Markdown（物理同一执行根）**：使用相对于当前文档的相对路径（如 `./sub/doc.md` 或 `../sibling.md`）。
- **跨执行根的本地记录**：使用经确认的执行宿主绝对路径（如 `/home/user/...`）。
- **简报与会话 Markdown**：使用经确认的执行宿主绝对路径作为 Markdown 链接或行内代码路径；未确认元数据时不要凭空捏造绝对路径。
- **公共渠道或共享文档**：优先使用便携的相对路径或公开 URL，默认不泄露特定机器的主机绝对路径。
- **传统 `workspace://` URI**：运行时与 Web GUI 解析器依然完全兼容历史格式（`workspace://<workspace_id>/<path>?root=<execution_root_id>`），但在新生成的 Agent 输出中不再作为默认推荐格式。

### 位置解析机制

运行时通过 `POST /api/file-references/resolve` 端点，将绝对路径、历史工作区 URI 以及带 `base_file` 的相对路径批量解析为已注册的工作区与执行根位置。解析器按文件系统锚点对多执行根进行去重，并保持规范工作区优先，确保跨 worktree 场景下的文件定位清晰可靠。

## 已知缺口

- 运行时任务拥有的 worktree 清理与 Agent 拥有的显式清理仍走各自独立的编排路径。
- workspace 占用是建议性的；运行时不在文件系统层面强制独占写访问。
- 托管 worktree 需要 git workspace，并通过 `git worktree add` 创建；运行时没有面向
  非 git 目录的隔离 workspace 路径。
