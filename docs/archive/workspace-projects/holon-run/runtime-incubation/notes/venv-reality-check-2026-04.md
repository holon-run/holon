# pre-public runtime v env 现实边界判断

日期：`2026-04-10`

## 背景

最近围绕 `pre-public runtime` 的 `v env`、sandbox、`workspace projection`、`credential store` 和 `Codex` 的默认安全模型做了一轮集中讨论与验证。

这轮讨论之后，最重要的收获不是“找到了一个现成 sandbox 方案”，而是：

- 哪些边界是当前阶段真实可做的
- 哪些边界如果继续抽象下去，会快速滑向“自己做操作系统”

## 先说结论

### 1. `pre-public runtime` 当前阶段不应该承诺“安全完备的 v env”

更准确地说：

- `pre-public runtime` 可以逐步收口 execution contract
- 也可以逐步引入更强 backend
- 但在当前阶段，不现实把 sandbox 抽象成一套“资源级、可观测、可交互授权、跨平台一致”的完备系统

这不是说这条路没有价值，而是说：

`当前最现实的交付物仍然是清晰 contract + 局部 hard boundary + 明确的软约束。`

### 2. `workspace-bound side effects` 仍然是最值得优先建模的部分

这部分包括：

- `workspace_entry`
- `workspace_anchor`
- `execution_root`
- `projection`
- `access_mode`
- `occupancy / lease`

这部分是 `pre-public runtime` 最核心、也最现实可控的副作用面。

### 3. 真正细粒度的资源仲裁已经接近“操作系统职责”

如果想做到下面这种能力：

- 任意进程访问文件时都能被拦截并回调上层应用
- 任意网络访问都能在运行时逐次授权
- 任意系统服务访问都能细粒度判定
- 凭据访问、socket 访问、daemon 访问统一在同一层管理

那本质上已经接近：

- OS sandbox
- userspace kernel
- virtualization runtime
- mediation layer

这不是 `pre-public runtime` 当前阶段应该自己承担的复杂度。

## 对 `Codex` 安全模型的观察

### 1. 默认模型更接近“读宽、写窄”

本地实际验证表明：

- 当前默认模型下，可以读取 workspace 外部源码目录
- 也可以读取 `~/.ssh`、`~/.config/gh` 这类用户 home 下路径
- 但向 workspace 外部路径写文件时，会触发授权

这说明：

`workspace-write` 本质上更接近 write-restricted sandbox，而不是 secret-safe read sandbox。`

### 2. 对任意 `bash/python`，`Codex` 不是先理解命令读写语义

对普通 shell 命令，`Codex` 主要不是：

- 先精确判断命令读还是写
- 再决定是否允许

而是：

- 让命令在一个受限 sandbox 里跑
- 如果它写到了不允许的地方，由 sandbox / OS 层拒绝
- 再根据 approval policy 决定是否进入升级授权

所以它更擅长：

- enforcement

不擅长：

- precise observation
- 每次资源访问的高层 hook

### 3. credential store 不是一等授权对象，而是“默认没放进白名单”

从 `Codex` 的 macOS Seatbelt 规则看：

- 默认 `deny default`
- 只显式放少量文件、网络、系统服务能力
- Keychain / credential-store 并没有被作为一等资源类暴露给上层

结果就是：

- 普通配置文件仍然可读
- 但依赖系统 credential store 的工具，如 `gh auth token`，在 sandbox 下可能失败

也就是说：

`当前隔离 credential store 主要靠 omission，而不是 first-class policy。`

## 这意味着什么

### 1. 仅靠当前这类 sandbox，不足以形成完整 secrets 模型

至少要区分：

- file-backed secrets
- system-backed secrets

二者不是同一种资源。

### 2. 让 credential store 永远不进白名单，也不是好方案

如果完全挡掉：

- `gh`
- 云厂商 CLI
- provider SDK

对系统 credential store 的访问，实际会逼用户转向：

- 环境变量
- 明文配置
- 更宽的无沙箱运行

这反而会降低整体安全性。

更合理的长期方向应该是：

`把 credential store 作为一等受保护资源，而不是永久禁区。`

但这已经超出了当前阶段的 `pre-public runtime` 能力边界。

## 对 `boxsh` 和类似方案的判断

### 1. `boxsh` 很适合解决文件副作用载体问题

它的价值在于：

- 提供 `cow:SRC:DST`
- 让写入落到明确副本目录
- 非 git workspace 也能形成隔离修改面

所以它很适合：

- `copied_root`
- COW workspace
- dry-run / discardable side effects

### 2. 但它不能自动变成完整的 `pre-public runtime v env`

因为它没有：

- `workspace_entry`
- `projection`
- `occupancy`
- `objective / continuation / trust`
- `credential store / socket / network` 的统一资源模型

所以它最多是：

- backend 候选

不是：

- `pre-public runtime` 上层 execution contract

## 当前阶段最现实的路线

### 1. 继续收口 runtime contract

优先把这些概念做扎实：

- `EnterWorkspace / ExitWorkspace`
- `projection = canonical_root | git_worktree_root | future copied_root`
- `access_mode = shared_read | exclusive_mutation`
- `occupancy / lease`

### 2. 对 agent 做软约束，而不是假装已有完备安全环境

当前阶段更现实的是：

- 在提示词中明确 workspace / projection / mutation 规则
- 用 runtime state 明确当前 `workspace_entry / execution_root / objective`
- 让 agent 在正确 contract 下工作

也就是说：

`当前阶段对 agent 的主要约束，仍然应以 prompt + runtime contract 为主。`

不是：

`先承诺已经有一个安全完备的 v env。`

### 3. 把 hard boundary 限定在少数真实可做的面上

例如：

- workspace attachment
- execution root selection
- worktree / copied root projection
- control-plane mutation surfaces
- callback / ingress / provenance marking

而不是急着承诺：

- 任意文件访问都可观测
- 任意网络访问都可交互授权
- 任意系统 credential store 都有统一 policy hook

## 一句话结论

`pre-public runtime` 当前阶段最现实的路线，不是先做“安全完备的 v env”，而是先把 `workspace-bound side effects` 和 runtime contract 收清楚，并通过提示词和显式 runtime state 软约束 agent。`

`真正细粒度、资源级、交互式的安全边界，已经明显接近操作系统 / userspace kernel / virtualization runtime 的职责，不适合作为当前阶段的主交付物。`
