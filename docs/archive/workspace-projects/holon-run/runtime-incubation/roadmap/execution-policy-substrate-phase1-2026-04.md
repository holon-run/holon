# pre-public runtime Execution Policy Substrate Phase 1

日期：`2026-04-04`

## 目标

在不引入 per-command approval、不实现容器/远程 backend 的前提下，
把 `pre-public runtime` 的执行边界从零散 host access 收口成一层最小可用的 execution substrate。

这不是完整的 `virtual execution environment`。

它只是第一阶段：

- 明确 `ExecutionProfile`
- 明确 `WorkspaceView`
- 统一 `ProcessHost`
- 让后续 `FileHost` 和 sandbox backend 有稳定接入点

## 非目标

这阶段不做：

- 容器 backend
- remote backend
- fake filesystem
- fake POSIX
- per-command approval UX
- 复杂 multi-profile matrix

## 交付物

### 1. `ExecutionProfile`

第一版建议只保留：

- `read_only`
- `workspace_write`
- `worktree_write`
- `background_worker`

约束：

- profile 绑定到 `agent`
- `task` 只能收窄 profile
- `worktree` 只能改变 workspace projection

### 2. `WorkspaceView`

负责：

- workspace root
- optional worktree root
- path resolution / projection

不负责：

- 审批
- profile ownership

### 3. `ProcessHost`

提供最小能力：

- `run`
- `spawn_background`
- `stop`

输入至少包含：

- `ExecutionProfile`
- `WorkspaceView`
- command / cwd / shell metadata

### 4. local backend

第一阶段唯一 backend：

- `LocalProcessHost`

要求：

- 行为尽量兼容当前测试
- 先不引入 sandbox backend
- 但接口设计要允许后续接 `sandbox-runtime` / `bubblewrap`

## 代码切入顺序

### 第一步

新增 execution substrate 模块，例如：

- `src/execution/mod.rs`
- `src/execution/profile.rs`
- `src/execution/workspace.rs`
- `src/execution/process.rs`
- `src/execution/local.rs`

### 第二步

把这些调用点改走 `ProcessHost`：

- `src/tool/execute.rs`
- `src/runtime/command_task.rs`
- `src/runtime/worktree.rs`
- `src/runtime/subagent.rs`

这是第一阶段最重要的收口动作。

### 第三步

把 agent 初始化路径绑上 base profile。

可能涉及：

- `src/host.rs`
- `src/runtime.rs`

### 第四步

再决定文件层是否需要：

- 先做轻量 `WorkspaceView`
- 还是继续追加薄 `FileHost`

这一步不必和 process 层同时开工。

## 验收标准

- 生产代码里不再到处直接 `Command::new(...)` 执行 agent-facing 进程
- process execution 都经过统一入口
- agent 拥有显式 base `ExecutionProfile`
- task/worktree 对 profile/view 的作用是显式的
- 当前本地 backend 行为保持兼容

## 延后判断点

以下问题不需要现在定死：

- `FileHost` 是否应成为与 `ProcessHost` 同等重量的一等接口
- `cap-std` 是否直接进入第一阶段
- `sandbox-runtime` 是不是 Phase 2 backend
- persistence 层需要持久化多少 execution metadata

## 备注

如果 Phase 1 做完后发现：

- `WorkspaceView` 已足够覆盖文件层大部分问题
- `ProcessHost` 已经把最主要的执行边界收口

那 `FileHost` 完全可以继续延后，不必为了对称性硬做。
