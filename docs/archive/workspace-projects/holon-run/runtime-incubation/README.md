# runtime-incubation

> Archived from `workspace/projects/holon-run/` during the Holon Rust runtime
> migration. The pre-public runtime incubation name and paths were normalized
> for this public repository; this directory preserves product, benchmark, and
> strategy context rather than exact private-source paths.

`projects/holon-run/runtime-incubation/` 保存 `pre-public runtime` 相关的非源码型材料，尤其是：

- 不适合直接放进源码仓库的方向判断
- 对外部项目和现成库的调研记录
- 阶段性实施草案和架构决策 memo

## 基本信息

- `GitHub repo`: `https://github.com/holon-run/runtime-incubation`
- `Local path`: `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation`
- `定位`: agent-first runtime substrate / 长期运行的 agent service

## 当前核心问题

当前最值得持续跟踪的问题，不是单个 tool 能力，而是：

- `pre-public runtime` 是否应该继续走 agent-bound execution profile 这条路
- 这条路与 `Codex` / `Claude Code` 的 sandbox + approval 方案相比，边界和风险在哪里
- 哪些第三方库可以直接复用，哪些必须由 `pre-public runtime` 自己定义

## 入口文件

- [Benchmark framework: manifest and naming（2026-04）](./notes/benchmark-framework-manifest-and-naming-2026-04.md)
- [执行边界调研与现成库评估（2026-04）](./notes/execution-substrate-evaluation-2026-04.md)
- [Sandbox / v env 候选方案比较（2026-04）](./notes/sandbox-options-comparison-2026-04.md)
- [v env 现实边界判断（2026-04）](./notes/venv-reality-check-2026-04.md)
- [Backend-mediated execution 方向判断（2026-04）](./notes/backend-mediated-execution-direction-2026-04.md)
- [pre-public runtime dogfooding 行动项（2026-04）](./notes/dogfooding-action-items-2026-04.md)
- [pre-public runtime dogfooding 复盘（2026-04）](./notes/dogfooding-retrospective-2026-04.md)
- [Task / Agent Surface 收敛判断（2026-04）](./notes/task-agent-surface-convergence-2026-04.md)
- [Session handoff（2026-04-06）](./notes/session-handoff-2026-04-06.md)
- [Session handoff（2026-04-14）](./notes/session-handoff-2026-04-14.md)
- [Execution Policy Substrate Phase 1（2026-04）](./roadmap/execution-policy-substrate-phase1-2026-04.md)
- [Public Contract Baseline（2026-04）](./roadmap/public-contract-baseline-2026-04.md)
- [公开前里程碑 Memo（2026-04）](./roadmap/public-readiness-milestones-2026-04.md)
- [Substrate-First 任务拆解（2026-04）](./roadmap/substrate-first-task-breakdown-2026-04.md)
- [Issue Drafts（Substrate-First，2026-04）](./roadmap/issue-drafts-substrate-first-2026-04.md)
- [RFC: Result Closure Contract（2026-04）](./roadmap/rfc-result-closure-contract-2026-04.md)

## 当前判断

- `pre-public runtime` 不适合照搬 `Codex` / `Claude Code` 的逐命令审批模型
- 对长期运行 agent 来说，`agent-level execution profile` 仍然是更合理的方向
- 但第一阶段不该直接做“大而全的 virtual execution environment”
- 更稳的顺序是：
  - 先做 `ExecutionProfile`
  - 再做 `WorkspaceView`
  - 优先收口 `ProcessHost`
  - `FileHost` 先保持轻量

## 维护约定

- 这里优先写“判断”和“路线”，不是重复源码里的接口定义
- 如果某份文档已经沉淀为稳定产品/架构结论，再考虑回迁到源码仓库
- 如果只是阶段性试探、对外部项目的比较、候选方案排除过程，优先留在这里
