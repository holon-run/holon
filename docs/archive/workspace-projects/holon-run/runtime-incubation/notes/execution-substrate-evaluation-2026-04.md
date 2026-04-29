# pre-public runtime 执行边界调研与现成库评估

日期：`2026-04-04`

## 背景

`pre-public runtime` 当前已经完成：

- `session-first -> agent-first` 的内部重构
- runtime 边界初步拆分

下一步原本计划进入：

- `virtual execution environment`
- `ExecutionProfile`
- `FileHost / ProcessHost / WorkspaceView`

但在真正开工前，需要先重新回答两个问题：

1. 这条路是不是方向正确
2. 是否已有足够成熟的现成库，可以避免自己造一整套轮子

## 先说结论

### 1. `pre-public runtime` 不应该照搬 `Codex` / `Claude Code` 的逐命令审批模式

原因不是这些产品做错了，而是它们解决的是另一类问题。

它们的典型形态是：

- 本地交互式 coding assistant
- 默认有人类在回路里
- 风险控制主要依赖 sandbox 和 approval UX

而 `pre-public runtime` 想要的是：

- 长期运行的 agent service
- background task / callback / trigger 驱动
- child-agent / worktree / durable recovery

在这种形态下，`per-command approval` 会直接破坏长期自治，不适合作为主模型。

### 2. `agent-level execution profile` 方向仍然成立

所以原来文档里的核心判断没有错：

- 执行权限应该绑定在 `agent`
- `task` 只能临时收窄
- `worktree` 是 workspace projection，不是权限主体

这条路不是在模仿今天最常见的 coding CLI 产品，而是在为“长期 agent runtime”做基础设施。

### 3. 但第一阶段不应该直接做“大而全的 virtual execution environment”

真正稳的第一阶段应该是：

- `ExecutionProfile`
- `WorkspaceView`
- `ProcessHost`

然后：

- `FileHost` 先保持轻量
- 容器 backend / remote backend 先不做
- 不去实现 fake filesystem / fake POSIX

## 对 `Codex` 的观察

本地可见代码表明，`Codex` 明显是：

- `sandbox policy`
- `approval mode`
- 平台后端实现

这条路线。

能看到的关键点：

- CLI 直接暴露 `--sandbox` 和 `--ask-for-approval`
- 状态层持久化 `sandbox_policy` / `approval_mode`
- Linux 侧自己维护 `bubblewrap + seccomp + landlock`

这说明：

- `Codex` 非常重视 execution boundary
- 但它不是先做通用 `agent execution substrate`
- 而是做“面向产品交互”的 sandbox/policy 系统

对 `pre-public runtime` 的启发是：

- 要重视 execution boundary
- 但不必复制它的 approval UX
- 更不必把 `pre-public runtime` 变成另一个本地 CLI sandbox 产品

## 对 `Claude Code` 的观察

本地能看到的材料主要是安装内容和一个本地源码镜像，不是 Anthropic 官方完整开源仓库，所以这部分只能作为辅助参考。

从代码形态看，`Claude Code` 也更像：

- `SandboxManager`
- `PermissionMode`
- `BashTool` 级别的 permission / readonly / sandbox 判断
- 本地 bash task 的任务化管理

这说明它解决问题的方式更接近：

- tool-specific permission orchestration
- sandbox runtime integration
- 人在回路里的 permission flow

而不是 `pre-public runtime` 想要的 runtime-substrate 抽象。

## 现成库调研

### `cap-std`

链接：

- `https://github.com/bytecodealliance/cap-std`

适合程度：`高`

适合场景：

- `WorkspaceView`
- capability-based 文件访问
- 把“裸路径 + workspace_root 判断”提升成“持有目录能力后再做相对访问”

优点：

- 和 `pre-public runtime` 的 `workspace projection` 思路高度一致
- 非常适合做轻量 `FileHost` 或 `WorkspaceView`
- 可以显著减少 path escape / 路径语义散落在各处的问题

限制：

- 它不是完整 sandbox
- 它不提供长期 agent 的 profile / overlay 语义
- 它也不是通用 process policy 框架

判断：

- 值得优先评估
- 更像 `pre-public runtime` 文件层的基础件，而不是整套 execution substrate

### `pathrs`

链接：

- `https://docs.rs/pathrs`

适合程度：`中高`

适合场景：

- Linux-first 的安全路径解析
- 在不可信目录树里做更强的 in-root path handling

优点：

- 对路径安全问题非常专注
- 很适合作为 Linux 下更强的 path resolution primitive

限制：

- Linux only
- 主要解决路径和文件句柄，不解决 process host
- 不提供 agent profile / task overlay 模型

判断：

- 如果 `pre-public runtime` 后面要在 Linux 上做更强约束，很值得保留
- 但不适合作为第一阶段唯一基础

### `sandbox-runtime`

链接：

- `https://github.com/anthropic-experimental/sandbox-runtime`
- `https://docs.rs/sandbox-runtime`

适合程度：`中高`

适合场景：

- `ProcessHost` 的 sandboxed backend
- macOS / Linux 下的 OS-level sandbox

优点：

- 明显就是 arbitrary process sandbox backend
- macOS 用 seatbelt
- Linux 用 bubblewrap + seccomp
- 比自己直接从头接系统原语轻一些

限制：

- 它是 backend，不是 `pre-public runtime` 的上层 execution model
- 不会帮你定义 `ExecutionProfile`
- 不会帮你定义 `task overlay` / `worktree projection`

判断：

- 很值得作为 future `ProcessHost` backend 候选
- 但第一阶段不应该先让上层架构依赖它的具体模型

### `bubblewrap`

链接：

- `https://github.com/containers/bubblewrap`

适合程度：`中`

适合场景：

- Linux sandbox backend

优点：

- 成熟
- 适合做 filesystem / namespace 隔离

限制：

- 太底层
- 只解决 Linux 侧部分问题
- 不负责 `pre-public runtime` 的 agent semantics

判断：

- 应作为 Linux backend primitive 看待
- 不应成为 `pre-public runtime` 高层 execution architecture 的起点

### `landlock`

链接：

- `https://docs.rs/landlock`

适合程度：`中`

适合场景：

- Linux 自我收缩型文件访问限制

优点：

- 轻量
- 与 capability/security 模型相容

限制：

- Linux only
- 更接近 sandbox primitive，而不是 runtime abstraction

判断：

- 适合后续 Linux 强化层
- 不适合作为第一阶段的中心设计

### `Wasmtime / WASI`

链接：

- `https://docs.wasmtime.dev/security.html`

适合程度：`低到中`

适合场景：

- 运行受控 WASI 程序
- 更强的插件/子程序沙箱

优点：

- capability-based model 非常清晰
- 对隔离很强

限制：

- 对现有 `git` / `bash` / shell tooling 基本不直接适用
- 会把 `pre-public runtime` 的工具生态约束得过重

判断：

- 不是当前主线
- 未来只可能作为特定受控 worker backend

## 目前最合理的分层

### 推荐先做

- `ExecutionProfile`
- `WorkspaceView`
- `ProcessHost`

### 可以后做

- 轻量 `FileHost`
- 更强路径安全 primitive
- sandbox backend

### 暂时不要做

- fake filesystem
- fake POSIX
- 容器 backend
- remote backend
- per-command approval UX

## 第一阶段建议蓝图

### `ExecutionProfile`

第一版只保留少量 profile：

- `read_only`
- `workspace_write`
- `worktree_write`
- `background_worker`

注意：

- profile 数量一定要少
- profile 是稳定 agent 属性
- task 只能在此基础上收窄

### `WorkspaceView`

负责：

- 当前 agent 的可见 workspace root
- optional worktree projection
- path resolution 入口

不负责：

- 权限拥有
- 安全策略总控

### `ProcessHost`

第一阶段最重要。

优先收口这些调用点：

- `exec_command`
- `command_task`
- worktree 里的 `git`
- subagent / worktree 创建过程里的 `git`

目标不是立刻强隔离，而是：

- 把 process execution 入口统一
- 把 profile/workspace 信息都显式传进去
- 为以后接 sandbox backend 留稳定边界

### `FileHost`

先做薄层即可。

它第一阶段只需要帮助统一：

- path resolution
- workspace projection
- 读写策略入口

不需要一开始就做大而全的可替换 backend 框架。

## 主要风险

### 1. 抽象过早

如果先设计出一整套对称、漂亮的 `FileHost + ProcessHost + IngressHost` 体系，
很容易在只有本地 backend 的阶段就过度建模。

### 2. profile 爆炸

如果第一版 profile 太多，后面会迅速变成 policy spaghetti。

### 3. 把 worktree 误当成权限主体

这会把“环境投影”和“权限拥有者”混在一起，让 runtime 边界再次变脏。

### 4. 把 backend primitive 反向上升为产品模型

例如太早让 `bubblewrap` / `landlock` / `sandbox-runtime` 的细节形状决定 `pre-public runtime` 的 API。

### 5. persistence 被绑死

如果太早持久化 substrate 细节，后面 `#1 persistence` 会被迫兼容一堆早期实验接口。

## 最终建议

对 `pre-public runtime` 来说，更稳的表达不是：

- “现在实现完整 virtual execution environment”

而是：

- “先建立面向长期 agent 的 execution policy substrate”

具体含义是：

- 保留 `agent-level execution profile`
- 优先统一 process boundary
- 文件层先轻量收口
- backend 可替换，但第一阶段只做 local backend

这条路线不是在复制今天主流 coding assistant 的产品实现，
而是在为长期运行 agent service 提前搭一层正确的执行边界。
