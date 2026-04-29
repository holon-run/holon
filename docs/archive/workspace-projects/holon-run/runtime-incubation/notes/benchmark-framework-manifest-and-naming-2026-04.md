# pre-public runtime Benchmark Framework: Manifest and Naming

Date: 2026-04-06
Scope: benchmark framework design, phase 1

## Goal

为 `pre-public runtime vs Codex` / `pre-public runtime vs Claude Agent SDK` 的对比实验先固定两件最关键的东西：

1. benchmark task manifest
2. branch / worktree / PR / label 命名规则

这一版先不定义完整 runner implementation，只定义足够稳定的输入合同和命名约束。

## Design Principles

- 主 benchmark 用真实仓库、真实任务，不用 synthetic mock task 作为主结论来源
- 同一个 task 必须能被多个 runner 复用
- 所有 runner 必须从同一个 `base_sha` 起跑
- 结果必须能被后续脚本和人类 review 同时读取
- 命名需要做到：
  - 一眼知道任务来源
  - 一眼知道 runner
  - 一眼知道这是 benchmark PR，而不是正常开发 PR

## Part 1: Benchmark Task Manifest

### Purpose

`task manifest` 是每个 benchmark 任务的标准输入。

它的职责不是承载全部运行时状态，而是固定：

- 任务来自哪个 repo / issue
- 所有 runner 共享的任务说明
- 统一的验收标准
- 允许的改动范围
- 时间和轮次预算

这样可以最大限度减少：

- 某个 runner 拿到的任务描述更宽
- 某个 runner 从不同 commit 起跑
- 某个 runner 有额外人工解释

### Storage layout

建议统一放到一个独立目录，例如：

```text
benchmarks/tasks/<task_id>.yaml
```

例如：

```text
benchmarks/tasks/runtime-incubation-0015-tool-guidance-registry.yaml
benchmarks/tasks/runtime-incubation-0019-prompt-section-identity.yaml
```

### Required fields

下面是建议的最小字段集合。

```yaml
schema_version: 1
task_id: runtime-incubation-0015-tool-guidance-registry
repo:
  name: holon-run/runtime-incubation
  local_path: /abs/path/to/repo
issue:
  number: 15
  title: Dogfood: extract a tool guidance registry from the prompt assembly
base:
  branch: main
  sha: 69ae3d4493ed43c3fa3e96ef9b5d4cb3fb7e84f0
task:
  kind: implementation
  prompt: |
    Extract a tool guidance registry from the prompt assembly.
    Keep behavior stable.
acceptance:
  summary: |
    Tool guidance is registry-backed, existing behavior is preserved,
    and the targeted test command passes.
  verification_commands:
    - cargo test --manifest-path Cargo.toml prompt::tools::
edit_scope:
  allowed_paths:
    - src/prompt
    - src/tool
  forbidden_paths:
    - src/runtime
budget:
  max_minutes: 90
  max_operator_followups: 3
review:
  mode: standardized
  expected_comment_count: 2
metadata:
  difficulty: medium
  benchmark_group: prompt-system
```

### Field semantics

#### `schema_version`

用于后续迭代 manifest 结构，不要省略。

#### `task_id`

benchmark 内部唯一 ID。

建议格式：

```text
<repo-short>-<issue-number-zero-padded>-<slug>
```

例如：

```text
runtime-incubation-0015-tool-guidance-registry
```

#### `repo`

定义 benchmark 针对的真实仓库。

建议至少包含：

- `name`
- `local_path`

#### `issue`

保留真实 issue 的编号和标题。

这样 benchmark 结果可以回链接到原始 backlog。

#### `base`

最关键的公平性字段。

必须固定：

- `branch`
- `sha`

所有 runner 都必须从同一个 `sha` 起跑。

#### `task`

这里放共享的任务说明。

建议保留：

- `kind`
  - `implementation`
  - `review_fix`
  - `continuation`
  - `documentation`
- `prompt`

`prompt` 应该是 benchmark driver 传给所有 runner 的 canonical task brief。

#### `acceptance`

定义什么叫“完成”。

建议至少有：

- `summary`
- `verification_commands`

后续如果要支持更强的比较，可以再加：

- `expected_files`
- `expected_behavior`
- `must_not_change`

#### `edit_scope`

这个字段很重要，因为很多 benchmark 任务本质上是在比较 scope discipline。

建议至少有：

- `allowed_paths`
- `forbidden_paths`

#### `budget`

预算字段用于控制 benchmark 的监督成本。

建议至少有：

- `max_minutes`
- `max_operator_followups`

后续也可以扩展：

- `max_review_rounds`
- `max_verification_commands`

#### `review`

如果任务要进入标准化 review repair 阶段，这里可以先声明预期模式。

建议至少保留：

- `mode`
  - `none`
  - `standardized`
  - `real_human`
- `expected_comment_count`

#### `metadata`

放一些不会影响语义，但方便后续聚合分析的信息，例如：

- `difficulty`
- `benchmark_group`

### Optional fields

这些可以先不做，但建议预留：

```yaml
operator_protocol:
  style: narrow
  single_goal_followups: true
environment:
  env:
    PRE-PUBLIC RUNTIME_HOME: /tmp/bench/...
artifacts:
  expected_pr: true
  expected_worktree: true
comparison:
  pair_group: openai-runtime
```

### Manifest invariants

为了保证对比有意义，manifest 需要满足这些约束：

1. 一个 task manifest 只能对应一个真实 issue 或一个明确的真实 follow-up work item
2. `base.sha` 一旦确定，在一轮 benchmark 中不得变动
3. `prompt` 必须是所有 runner 的 canonical 输入，不允许对某个 runner 私下补充更多任务信息
4. 如果某个 runner 需要额外解释，这个解释必须写回 benchmark 记录，而不能隐式注入口头上下文

## Part 2: Naming Conventions

### Goal

让同一个任务在多 runner 并行时仍然可读、可查、可清理。

要解决的问题：

- 同一 issue 可能同时出现 4 个分支
- 同一 issue 可能同时出现 4 个 draft PR
- 需要从名字一眼判断：
  - 这是哪个 runner
  - 这是哪个 task
  - 这是不是 benchmark 产物

### Canonical runner ids

建议先固定 4 个 runner id：

- `runtime-incubation-openai`
- `codex-openai`
- `runtime-incubation-claude`
- `claude-sdk-claude`

如果后续加别的模型或别的系统，再扩展，不要先做抽象枚举系统。

### Branch naming

建议格式：

```text
bench/<task_id>/<runner_id>
```

例如：

```text
bench/runtime-incubation-0015-tool-guidance-registry/runtime-incubation-openai
bench/runtime-incubation-0015-tool-guidance-registry/codex-openai
bench/runtime-incubation-0015-tool-guidance-registry/runtime-incubation-claude
bench/runtime-incubation-0015-tool-guidance-registry/claude-sdk-claude
```

优点：

- 按 task 聚类
- 一眼能看到 runner
- 方便批量清理 `bench/*`

### Worktree naming

建议 worktree 目录名尽量短，但保留同样信息。

格式：

```text
bench-<issue-number>-<runner-short>
```

例如：

```text
bench-15-runtime-incubation-openai
bench-15-codex-openai
bench-15-runtime-incubation-claude
bench-15-claude-sdk
```

如果需要更稳定，也可以用：

```text
bench-0015-runtime-incubation-openai
```

建议所有 benchmark worktree 挂到统一根目录下，例如：

```text
/tmp/bench-worktrees/<repo>/<worktree-name>
```

### PR title naming

建议格式：

```text
[bench][<runner_id>][#<issue_number>] <issue_title>
```

例如：

```text
[bench][runtime-incubation-openai][#15] Dogfood: extract a tool guidance registry from the prompt assembly
```

这样在 GitHub 列表页里就能直接看出：

- 这是 benchmark PR
- 它属于哪个 runner
- 它对应哪个真实 issue

### PR body naming block

PR body 开头建议统一一个 metadata block：

```md
Benchmark metadata:

- task_id: `runtime-incubation-0015-tool-guidance-registry`
- runner: `runtime-incubation-openai`
- base_sha: `69ae3d4493ed43c3fa3e96ef9b5d4cb3fb7e84f0`
- issue: `#15`
- benchmark_mode: `initial_implementation`
```

这样后续 review 和结果归档更简单。

### Label naming

建议至少固定这些 label：

- `bench`
- `bench:task-15`
- `runner:runtime-incubation-openai`

如果后续要区分轮次，也可以再加：

- `bench:phase-implementation`
- `bench:phase-repair`

### Artifact naming

每个 runner 的输出目录建议统一：

```text
artifacts/<task_id>/<runner_id>/
```

例如：

```text
artifacts/runtime-incubation-0015-tool-guidance-registry/runtime-incubation-openai/
```

其中可以放：

- transcript
- logs
- timings
- final summary
- review comments

### Naming invariants

必须遵守：

1. 同一个 `task_id` 在所有命名表面都保持一致
2. `runner_id` 必须来自 canonical runner list，不允许一会儿叫 `claude-sdk` 一会儿叫 `claude-code-sdk`
3. 所有 benchmark PR 必须能从标题直接识别，不允许伪装成正常开发 PR

## Recommended MVP

如果只先做最小版本，我建议先固定这 5 件事：

1. `task_id` 规则
2. manifest 的 required fields
3. branch 命名规则
4. PR title 规则
5. label 规则

只要这 5 件事固定下来，后面的 runner 和 collector 就容易收口。

## Open Questions

这一版暂时不解决：

- benchmark driver 的具体实现语言
- PR 是 `draft` 还是普通 PR
- review comment 如何标准化注入
- 结果汇总 schema 长什么样

这些适合放到下一份文档里继续定义。
