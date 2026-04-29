# pre-public runtime sandbox / v env 候选方案比较

日期：`2026-04-10`

## 背景

在 `pre-public runtime` 当前的执行边界讨论里，已经逐步收敛出几个判断：

- `workspace` 是副作用载体，不只是路径根
- `v env` 不是孤立的计算盒子，而是 `execution environment + side-effect carrier binding`
- Phase 1 的现实 backend 应该先是 `host_local`
- `git_worktree_root` 不是 backend，而是 `host_local` 下的一种 projection

这意味着，评估 sandbox 方案时，不能只问“能不能隔离进程”，还要问：

1. 能不能和 `workspace / execution_root / projection` 绑定
2. 能不能承载文件副作用，而不是只做短暂计算
3. 能不能适配 `pre-public runtime` 的 local-first、long-lived、coding-oriented 形态

## 先说结论

### 1. `pre-public runtime` 不应该把 sandbox 方案直接等同于自己的 execution model

`pre-public runtime` 仍然需要自己定义：

- `workspace_entry`
- `workspace_anchor`
- `execution_root`
- `projection`
- `access_mode`
- `occupancy / lease`

外部 sandbox 方案最多只应成为：

- `v env backend`
- 或 backend primitive

不应反过来决定 `pre-public runtime` 的上层 runtime contract。

### 2. 当前最值得认真评估的，不是一个方案，而是两条线

#### A. `sandbox-runtime`

更像：

- arbitrary process sandbox backend
- 适合 future `ProcessHost` backend
- 跨 macOS / Linux

它的价值在于：

- 给 `pre-public runtime` 一个相对现成的 process containment backend

但它不提供：

- workspace projection 模型
- side-effect carrier 模型
- `pre-public runtime` 自己的 execution contract

#### B. `boxsh`

更像：

- `host_local + copied_root` 候选 backend
- 自带 shell / file tools / COW workspace
- 非 git workspace 也可用

它的价值在于：

- 为 `pre-public runtime` 提供一种明确的副作用载体：`src -> dst` 的 copy-on-write 工作区

但它不提供：

- `pre-public runtime` 的 `workspace_entry / projection / occupancy` 语义
- git-aware worktree 模型

### 3. Linux 原语和 Linux-only launcher 不应成为 pre-public runtime 的第一层抽象

像这些更适合看成：

- `bubblewrap`
- `landlock`
- `nsjail`
- `firejail`

它们可以是：

- Linux backend primitive
- 或 Linux-only backend implementation candidate

但不适合作为 `pre-public runtime` 的跨平台主 execution abstraction。

### 4. `gVisor` / `Firecracker` 更像 Phase 2 甚至更后的路线

原因不是它们不强，而是：

- 太偏 container / microVM
- 更适合托管环境或强隔离多租户
- 和 `pre-public runtime` 当前的 local-first、workspace-bound 副作用模型衔接成本高

## 比较维度

这里重点比较 6 个维度：

1. `OS support`
2. `Isolation strength`
3. `Workspace-bound side effects`
4. `Git / non-git workspace fit`
5. `Integration shape for pre-public runtime`
6. `Phase fit`

## 候选方案

### 1. `boxsh`

形态：

- sandboxed POSIX shell
- MCP server
- built-in file tools
- copy-on-write workspace

本地源码：

- `/Users/jolestar/opensource/src/github.com/xicilion/boxsh`

明显优点：

- 同时支持 shell 和文件工具
- 原生支持 `cow:SRC:DST`
- 非 git workspace 也能形成明确副作用载体
- Linux 和 macOS 都有实现
- 很适合做一次性隔离修改、dry-run、并行隔离 worker

明显限制：

- 它的核心模型是 `src -> dst`，更像 `copied_root` / COW root
- 不是 `pre-public runtime` 现在在讨论的 `canonical_root / git_worktree_root`
- 自己已经带 MCP server 和 tools，和 `pre-public runtime` 存在职责重叠
- macOS 依赖 `sandbox_init()` 这类私有/不稳定 API，长期风险偏高
- 没有 git-aware projection

对 `pre-public runtime` 的判断：

- 值得做 PoC
- 适合作为 `copied_root` 候选 backend
- 不适合作为 `pre-public runtime` sandbox abstraction 本身

阶段建议：

- `Phase 1.5 / Phase 2` 可评估
- 不是最应该先接入的唯一 backend

### 2. `sandbox-runtime`

形态：

- arbitrary process sandbox backend
- 面向 macOS / Linux 的 OS-level sandbox 封装

已知特点：

- macOS 走 seatbelt
- Linux 走 `bubblewrap + seccomp`

明显优点：

- 和 `pre-public runtime` 需要的 `ProcessHost` backend 形态接近
- 比直接从零拼接平台原语更轻
- 对 arbitrary process execution 更自然

明显限制：

- 它是 backend，不是 runtime model
- 不提供 `workspace_entry / projection / occupancy`
- 不提供 side-effect carrier 抽象

对 `pre-public runtime` 的判断：

- 是最值得认真评估的 future `ProcessHost` backend 候选之一
- 尤其适合“统一 arbitrary process containment”

阶段建议：

- `Phase 2` 候选
- 先不要让上层 contract 被它绑死

### 3. `bubblewrap`

形态：

- Linux user namespace / mount namespace sandbox launcher

明显优点：

- 成熟
- Linux sandbox 常用基础件
- 对 filesystem / namespace 隔离很实用

明显限制：

- Linux-only
- 太底层
- 只解决 backend primitive，不解决 `pre-public runtime` execution model
- 对 side-effect carrier 没有自己的抽象

对 `pre-public runtime` 的判断：

- 应视为 Linux backend primitive
- 不应成为 `pre-public runtime` 高层 API 的起点

阶段建议：

- `Phase 2 backend primitive`

### 4. `nsjail`

形态：

- Linux-only 进程隔离 / sandbox launcher
- 常见于 untrusted code / contest / service confinement

明显优点：

- 隔离能力强
- seccomp / namespaces / cgroups 一类能力完整
- 对“跑不可信程序”场景成熟

明显限制：

- Linux-only
- 更偏服务/作业级 jail，不是 workspace-bound coding runtime
- 没有 `workspace projection` 概念
- 不天然适配 long-lived agent 的副作用载体模型

对 `pre-public runtime` 的判断：

- 可参考
- 但不适合作为 local-first 跨平台主线

阶段建议：

- 若后续 `pre-public runtime` 做 Linux 专用强隔离 backend，可再评估

### 5. `firejail`

形态：

- Linux desktop / app sandbox launcher

明显优点：

- 上手容易
- 对已有 Linux app 做快速约束方便

明显限制：

- Linux-only
- 更偏桌面程序和现成应用包装
- 对 `workspace-bound side effects` 没有第一类建模
- 不适合当 `pre-public runtime` 的核心 execution substrate

对 `pre-public runtime` 的判断：

- 参考价值有限
- 更像对现成 app 的 wrapper，而不是 agent runtime backend

阶段建议：

- 不建议作为主候选

### 6. `gVisor`

形态：

- 用户态 application kernel
- OCI runtime `runsc`

明显优点：

- 隔离强于普通 container primitive
- 保留较快启动和较低资源开销
- 对多租户和容器场景成熟

明显限制：

- 明显面向 OCI / container 生态
- 集成重量大于 `pre-public runtime` 当前 phase-1 需要
- 不天然绑定本地 workspace 副作用载体

对 `pre-public runtime` 的判断：

- 如果将来有托管 / container-native execution backend，值得再看
- 当前 local-first 阶段太重

阶段建议：

- `Phase 3+`

### 7. `Firecracker`

形态：

- microVM VMM
- KVM-based 强隔离环境

明显优点：

- 隔离最强的一档
- 启动快于传统 VM
- 多租户 / serverless 经验成熟

明显限制：

- Linux host / hardware virtualization 前提
- 运维和环境复杂度显著上升
- 和本地 workspace / projection 的映射成本高
- 不适合作为 `pre-public runtime` 当前 local-first 默认 execution backend

对 `pre-public runtime` 的判断：

- 更像未来托管版 / remote backend 研究方向
- 不是当前本地 runtime 的现实选择

阶段建议：

- `Phase 3+`

## 补充：原语 vs backend

这里还要明确一个边界：

- `landlock`
- `seccomp`
- `cap-std`
- `pathrs`

这些更像：

- safety primitive
- file/path capability primitive
- backend 实现细节

它们值得接入，但不应和：

- `boxsh`
- `sandbox-runtime`
- `gVisor`
- `Firecracker`

放在同一层比较。

## 对 pre-public runtime 的直接启发

### 1. 先坚持 `pre-public runtime` 自己的 workspace / execution contract

当前最重要的仍然是：

- `EnterWorkspace / ExitWorkspace`
- `workspace_entry`
- `projection = canonical_root | git_worktree_root | future copied_root`
- `access_mode = shared_read | exclusive_mutation`

这些不应交给外部 sandbox 方案来定义。

### 2. `boxsh` 最适合补 `copied_root`

如果 `pre-public runtime` 后面要支持：

- 非 git workspace 并行改
- 一次性隔离修改
- dry-run / discardable side effects

`boxsh` 是最值得做 PoC 的候选之一。

### 3. `sandbox-runtime` 最适合补 arbitrary process containment

如果 `pre-public runtime` 后面要强化：

- `ProcessHost` 的 sandboxed backend
- macOS / Linux 的统一 process containment

它是比直接从头拼 `bubblewrap + seatbelt` 更值得优先评估的路线。

### 4. Linux-only 方案先不要抬到主 contract

`bubblewrap / nsjail / firejail / landlock`

都很有用，但当前更适合：

- backend primitive
- platform-specific enhancement

不适合直接决定 `pre-public runtime` 的产品级 runtime 抽象。

## 推荐顺序

### 应优先继续的

1. 继续把 `pre-public runtime` 自己的 `workspace entry / projection / occupancy` 收口
2. 将来先 PoC 两条 backend 路线：
   - `sandbox-runtime` for arbitrary process containment
   - `boxsh` for `copied_root` / COW-root workspace backend

### 暂不优先的

1. 直接把 Linux-only sandbox 当跨平台主线
2. 直接上 container / microVM 方案
3. 让外部 sandbox 方案反过来定义 `pre-public runtime` 的 execution contract

## 一句话结论

`boxsh` 值得看，但更像 `copied_root` backend 候选；`sandbox-runtime` 更像通用 process sandbox backend 候选。
`pre-public runtime` 当前最该做的仍然不是选定唯一 sandbox，而是先把自己的 workspace / projection / occupancy contract 站稳。`
