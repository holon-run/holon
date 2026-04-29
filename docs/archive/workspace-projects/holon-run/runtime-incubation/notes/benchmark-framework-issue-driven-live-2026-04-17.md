# pre-public runtime Benchmark Framework: Issue-Driven Live Design

Date: 2026-04-17
Scope: live benchmark framework redesign after `#53` and `#160`

## Goal

把 live benchmark 收口成一个更稳定的 issue-driven 框架，只比较：

- `runtime-incubation-openai`
- `codex-openai`

这一版不再继续扩展 `claude-sdk` runner，也不再把 manifest 当成长篇 task brief。

目标是：

1. task 输入更接近真实使用
2. runner 拿到的任务合同更一致
3. private repo / grounded no-op / PR side effect 的处理更清楚
4. 比较结果更少受 harness 噪音影响

## Key Decisions

### 1. Runner scope

当前 live benchmark 只保留两条主要 runner：

- `runtime-incubation-openai`
- `codex-openai`

理由：

- 这两条是当前最可比的路径
- `claude-sdk` 目前没有真正可用的 resume / continuation 合同
- 把 `claude-sdk` 留在主 benchmark matrix 里，会把结论混进 runner capability 差异，而不是 agent quality 差异

后续如果要重新引入 `claude-sdk`，应单独做成实验 runner，而不是默认主矩阵的一部分

### 1.5 Runner execution model

`runtime-incubation-openai` 和 `codex-openai` 不应再按串行方式依次跑完。

新的默认执行模型应是：

- 同一个 task 下，`runtime-incubation-openai` 和 `codex-openai` 并行启动
- 两边都从同一个 `base_sha` 创建各自独立 worktree
- 两边分别产出自己的 result directory、branch、PR 和 summary

理由：

- 这两条 runner 现在是主比较对象，串行跑只会拉长 wall-clock 时间
- 它们本来就写入不同 worktree，不需要共享代码改动面
- 串行执行会把一个 runner 的超时、卡顿或 continuation 噪音放大成整轮 benchmark 延迟

因此，suite 层应默认把：

- 同一 task 的多 runner 比较

建模成：

- 一个 task-level fan-out
- 多个 isolated runner jobs
- 最后再做 task-level result aggregation

这里的目标不是一次性把整个 benchmark 调度器做成任意并发图，而是先把最有价值、最稳定的并行单元固定下来：

- `task x {runtime-incubation-openai, codex-openai}`

## Parallelization Contract

第一版并行合同建议收得很窄：

- 只并行 `runtime-incubation-openai` 和 `codex-openai`
- 不要求不同 task 之间也并行
- 不要求 continuation / review-fix 阶段和 phase 1 混并发

也就是说，推荐执行顺序是：

1. 取一个 task
2. 同时启动 `runtime-incubation-openai` 和 `codex-openai`
3. 等这两个 runner 都收口
4. 聚合该 task 结果
5. 再进入下一个 task

这样做的好处是：

- 并行收益已经足够大
- 结果归档和 task-level review 仍然容易读
- 不会把整个 harness 调度器复杂度一次性推高太多

### 2. Task source

live task 继续以 GitHub issue 作为唯一任务源。

也就是说，benchmark 不再把 issue 正文复制进 prompt，也不要求 manifest 持有一段长篇 canonical task brief。

benchmark 要测的是：

- agent 能不能读 issue
- agent 能不能抽取范围和验收条件
- agent 能不能在本地仓库里完成实现并收口

### 3. Prompt contract

runner prompt 不再只给裸 issue URL，也不再直接使用 manifest 里的长 `operator_prompt`。

统一改成一个最小 issue-driven 模板：

```text
Fix GitHub issue #{issue_number} in this repository.

Issue:
{issue_url}

Instructions:
- Use `gh` commands to inspect the issue and related GitHub context.
- Stay within the issue scope.
- Use local repository context when needed.
- Run the task verifier before stopping.
- If you make a real implementation, follow the PR submission policy below.

PR policy:
{rendered_pr_policy}
```

这条模板有几个关键点：

- 默认要求使用 `gh` 看 issue
- 不再把 private repo fetch 当成特殊 case 单独补丁
- 保持 prompt 简短，但把执行合同补完整

### 4. PR policy rendering

manifest 里仍然保留结构化布尔值：

```yaml
pr:
  submit: true
  draft: true
```

但 prompt 渲染时不直接输出布尔字面量，而是渲染成自然语言。

例如：

- `submit=true`, `draft=true`

```text
PR policy:
- Submit a pull request if you make a real implementation.
- Submit it as a draft pull request.
```

- `submit=true`, `draft=false`

```text
PR policy:
- Submit a pull request if you make a real implementation.
- Do not mark it as draft.
```

- `submit=false`

```text
PR policy:
- Do not submit a pull request automatically.
```

这样更像正常操作指令，不像配置文件。

## Manifest Changes

### Keep

manifest 继续保留这些字段：

```yaml
schema_version: 1
task_id: ...
repo:
  name: ...
  local_path: ...
issue:
  number: ...
  title: ...
base:
  branch: ...
  sha: ...
verification:
  commands:
    - ...
evaluation:
  expected_outcome: change_required | no_change_expected | either
  summary: ...
  scope_policy: soft | hard
  allowed_paths: []
  forbidden_paths: []
pr:
  submit: true | false
  draft: true | false
budget:
  max_minutes: ...
```

### Remove or de-emphasize

对于 live issue benchmark，`task.operator_prompt` 不再作为主要输入合同。

如果保留它，也只应作为：

- 任务元数据
- 调试/迁移兼容字段

而不是 runner 的直接 prompt 来源。

理由：

- issue 已经是 canonical task source
- 再维护一份独立长 prompt，容易和真实 issue 漂移
- 这轮 `#160` 已经证明，真正的问题不是“缺少长 prompt”，而是“issue-only 合同不完整”

## Success Model

`grounded no-op` 继续作为正式结果类型保留。

因此 success 判定应以：

- runner 是否成功完成
- verifier 是否通过
- 是否符合 `expected_outcome`
- 是否违反 scope policy

为主，而不是一律要求必须产生 diff。

建议继续使用：

- `change_required`
- `no_change_expected`
- `either`

这也是 live tasks 公平比较所必需的。

## Why Private Issue Fetch Is No Longer A Top-Level Framework Item

这一轮一开始看起来像是 private issue fetch 问题，但更准确地说，根因是：

- runner 只给了 issue URL
- prompt 里没有明确要求用 `gh`
- manifest 的完整 task context 又没有真正进入 prompt

在新设计下：

- prompt 明确要求用 `gh`
- issue 仍然是唯一任务源
- benchmark 不再依赖“通用网页抓取”才能拿到任务含义

所以“private issue fetch”不再需要作为独立高优先级框架改进项，而只是 issue-driven prompt 合同中的默认行为。

## Continuation Scope

这一版框架先不把 continuation 做成主设计中心。

原因：

- `runtime-incubation-openai` 和 `codex-openai` 都已经有可用 continuation 路径
- 但当前主 benchmark 先要解决的是：任务输入合同更稳

因此当前优先级是：

1. 统一 issue-driven prompt
2. 明确 PR policy
3. 保留 grounded no-op success model

continuation 仍重要，但应作为第二阶段增强，不应继续阻塞主框架重构。

## Migration Plan

### Phase 1

- 只保留 `runtime-incubation-openai` / `codex-openai`
- 同一个 task 下默认并行启动这两个 runner
- 把 `buildOperatorPrompt()` 改成 issue-driven模板
- 从 manifest 读取并渲染 `pr.submit` / `pr.draft`
- 不再依赖长 `operator_prompt`

### Phase 2

- 把 live benchmark summary 里显式记录：
  - PR policy
  - expected outcome
  - grounded no-op / real change / publish skipped

### Phase 3

- 如果需要，再把 continuation mode 作为显式 benchmark metadata 加回
- 若未来重引 `claude-sdk`，单独定义其能力边界，而不是并入默认主矩阵

## Recommended Next Changes

1. 修改 `benchmark/run.mjs` 的 `buildOperatorPrompt()`，使用统一 issue-driven模板。
2. 让同一 task 下的 `runtime-incubation-openai` / `codex-openai` 默认并行执行。
3. 给 manifest 增加稳定的 `pr.submit` / `pr.draft` 字段。
4. 继续保留 `expected_outcome` 的 success 模型，不回退到“必须有 diff”。
5. 从默认 suite 中移除 `claude-sdk`。

## Summary

这轮之后，live benchmark 的推荐方向是：

- 用 issue 做唯一任务源
- 用统一 issue prompt 模板补齐 `gh` / scope / PR contract
- 只比较 `runtime-incubation-openai` 和 `codex-openai`
- grounded no-op 作为正式有效结果保留

这比“长篇 operator prompt + 多 runner 混跑 + case-specific 修补”更接近真实使用，也更容易得到可解释的 benchmark 结论。
