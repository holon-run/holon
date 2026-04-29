# Backend-Mediated Execution Direction（2026-04）

## 结论

`pre-public runtime` 未来应该支持 `backend-mediated execution`，但当前阶段不应提前重构成完整多 backend 架构。

这里的 `backend-mediated execution` 指的是：

- workspace 是逻辑项目对象，不等于 host 本地目录
- shell / file / search / edit 等能力，最终都通过某个 execution backend 落地
- backend 可以是：
  - `host_local`
  - future `container`
  - future `ssh_remote`
  - future `copied_local`

## 为什么需要这个方向

当前就已经能看到两类未来形态：

1. `container` backend
workspace 映射进容器，agent 的 shell 实际在容器里执行；容器内命令集和 host 不一定一致。

2. `ssh_remote` backend
workspace 对应远程目录，agent 的 shell 和文件操作都通过远程执行完成。

如果 `pre-public runtime` 继续把 public contract 写死在：

- host 本地绝对路径
- host 本地 shell
- file tool 直接操作 host filesystem

那后续接入这些 backend 时会很难收口。

## 当前阶段不做什么

当前不建议：

- 立刻把所有 file tools 全部重构成 backend capability
- 立刻把所有 `PathBuf` 替换成逻辑路径抽象
- 立刻设计完整的 `container / ssh / copied` backend 矩阵
- 立刻把 `pre-public runtime` 变成通用 virtual execution platform

这会明显过度设计。

## 当前阶段应该记住什么

只保留一个方向性约束：

`不要把未来 public contract 锁死在 host-local 假设上。`

更具体地说：

- `workspace_entry` 继续表示逻辑项目对象
- `projection` 继续表示副作用载体
- `execution backend` 是未来需要补的一层，但现在先不强行全面落地
- file tools 不应在概念上永久等同于 shell 命令

## 当前最务实的做法

现阶段仍然优先：

- 把 workspace-bound side effects 的 runtime contract 做实
- 通过提示词和显式 runtime state 软约束 agent
- 接受当前主要还是 `host_local` 模型

等到第一次真的要接：

- `container` execution
- `ssh_remote` execution
- 或 `copied_root` backend

再把 `execution backend` 这一层正式抽出来。
