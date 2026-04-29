# pre-public runtime 测试现状盘点与改进计划（2026-04）

## 结论先行

pre-public runtime 当前不是“测试太少”，而是“测试已经很多，但结构上还不够均衡”。

我本地核对后的判断是：

- `cargo test -- --list` 当前可见 **581 个 Rust 测试**
- 其中已经覆盖了大量 runtime、context、prompt、tool、provider、daemon、TUI、HTTP control surface
- 另外还有：
  - `tests/live_openai.rs`
  - `tests/live_codex.rs`
  - `tests/live_anthropic.rs`
  - `benchmark/` 下的 benchmark runner tests
  - `workspace/projects/holon-run/runtime-incubation/benchmarks/` 下的一批 live benchmark manifests

所以 pre-public runtime 现在的真实问题不是“没有测试体系”，而是：

1. **大体量集成测试文件承担了过多职责**
2. **prompt / model-visible contract 还不够第一等公民**
3. **压缩边界 / waiting / reactivation 的矩阵还没有完全成型**
4. **CI 和 coverage 入口还比较粗**

一句话说：

> pre-public runtime 现在已经有了大量测试资产，下一阶段重点不是“继续堆测试数”，而是把现有测试体系重构成更清晰的分层。

## 1. 当前测试方式

## 1.1 Rust 单元测试

这层已经很扎实，主要分布在源码模块内。

代表性模块：

- `src/context.rs`
- `src/prompt/mod.rs`
- `src/prompt/tools.rs`
- `src/runtime/closure.rs`
- `src/runtime/continuation.rs`
- `src/runtime/memory_refresh.rs`
- `src/runtime/task_state_reducer.rs`
- `src/tool/dispatch.rs`
- `src/tool/execute.rs`
- `src/provider/tests.rs`
- `src/daemon/tests.rs`
- `src/tui.rs`
- `src/tui/input.rs`
- `src/tui/render.rs`
- `src/tui/projection.rs`

这层的特点：

- contract 较清晰
- 回归成本低
- 对边界值、序列化、payload shape、状态归约都测得比较细

优点：

- `context`、`prompt`、`runtime closure`、`tool dispatch` 这些核心合同已经开始稳定
- `tool schema`、`provider request/response parsing`、`daemon metadata` 这类很适合单测的模块已经比较完整

不足：

- 有些重要行为仍然只在大集成测试里验证，没有被压缩成更小的 contract tests

## 1.2 大型集成测试

当前最重的两份文件是：

- `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/tests/runtime_flow.rs`
- `/Users/jolestar/opensource/src/github.com/holon-run/runtime-incubation/tests/http_routes.rs`

文件体量：

- `runtime_flow.rs` 约 5400 行
- `http_routes.rs` 约 3200 行

这两份文件本质上是 pre-public runtime 现在的主测试骨架。

### `runtime_flow.rs`

这份文件已经覆盖了大量真正重要的主路径：

- message processing / brief / sleep
- turn-local compaction
- wake hint / timer tick / queued activation
- `NotifyOperator`
- callback / external trigger
- command task lifecycle
- subagent task lifecycle
- workspace / worktree lifecycle
- restart / recovery / follow-up behavior

优点：

- 覆盖面很强
- 很贴近真实 runtime 行为
- 对“改动一个 reducer，整个系统感觉变了”的问题有实际保护作用

问题：

- 文件太大，语义边界开始变模糊
- 失败时定位成本高
- 很难快速看出“哪部分是 waiting-plane、哪部分是 compaction、哪部分是 worktree”

### `http_routes.rs`

这份文件覆盖了当前 control / ingress / callback / operator transport 主路径：

- control prompt / wake / resume
- SSE / events replay
- public enqueue / generic webhook
- callback enqueue / wake-only
- operator ingress
- operator transport binding / delivery callback
- runtime status / shutdown
- workspace routes

优点：

- 基本把当前 HTTP boundary 的核心合同都兜住了

问题：

- 同样太大
- 一些逻辑更像“route contract”，适合拆成更小的专题文件

## 1.3 `run_once` 集成测试

`tests/run_once.rs` 当前覆盖：

- one-shot final text
- token usage surface
- runtime error delivery
- changed files aggregation
- no-wait / wait-for-task behavior
- max turns
- persistent named agent session
- template creation constraints
- workspace binding preservation

这层是好的，因为 `run` 是 pre-public runtime 最容易被用户直接感知的入口。

问题不大，主要是后面可以继续和 runtime 主线做 contract 对齐。

## 1.4 Worktree 专项测试

已有：

- `wt201_multiple_worktree_tasks.rs`
- `wt202_worktree_task_summary.rs`
- `wt203_task_owned_worktree_cleanup.rs`
- `wt204_parallel_worktree_workflow.rs`
- `wt205_worktree_lifecycle_edge_cases.rs`

这说明 worktree 这一层已经有单独专题测试，不再全塞进 `runtime_flow.rs`。

这是一个好的方向，说明 pre-public runtime 已经开始从“大一统文件”往专题测试拆。

## 1.5 Live provider tests

当前有：

- `tests/live_openai.rs`
- `tests/live_codex.rs`
- `tests/live_anthropic.rs`

这类测试的价值：

- 检查真实 provider contract 没漂移
- 检查最基础的真实请求链路还活着

风险：

- 不适合做主回归集
- 结果不稳定，成本高

它们应该保留，但定位应当是：

- smoke test
- provider compatibility canary

而不是主测试骨架。

## 1.6 Benchmark

pre-public runtime 当前还有一层和源码测试平行的验证：

- `benchmark/` runner tests
- `workspace/projects/holon-run/runtime-incubation/benchmarks/` manifests 和 suites

这层不是源码回归测试，而是：

- issue-driven live benchmark
- runner 间对比
- 行为级质量观测

这层非常有价值，但不应该拿来替代源码合同测试。

## 2. 当前覆盖程度判断

## 2.1 覆盖得比较好的部分

### Runtime 主干

这一层已经相当不错：

- waiting / wake / timer / continuation
- task lifecycle
- child/subagent lifecycle
- restart / recovery
- closure / posture / waiting reason

### Context / Memory / Prompt 的基础合同

这层也比预期更扎实：

- `context.rs` 有很多 budget、section ordering、active work、working memory 相关单测
- `prompt/mod.rs` 和 `prompt/tools.rs` 已经在测 contract emitted / omitted

### Tool / Provider contract

这层覆盖得很好：

- strict tool schema
- tool execution envelopes
- provider auth / fallback / transport parsing
- OpenAI/Codex request payload contract

### TUI 基础行为

TUI 已经不是空白：

- composer editing
- slash prompt
- projection
- render
- markdown

这块的单元测试比我原来预期更完整。

## 2.2 仍然偏弱的部分

### Prompt 还不是完整的 model-visible snapshot contract

现在有 prompt section tests，但主要还是：

- 某 section 是否出现
- 某 guidance 是否被包含

而不是像 Codex 那样：

- 对完整 model-visible layout 做 snapshot
- 对 prompt/context emitted order 做稳定回归

当前缺口主要在：

- system prompt 全量快照
- context prompt 全量快照
- event-turn / operator / external trigger / work item 不同输入面下的完整 prompt 布局对比

### Compression / Compaction 还缺“边界矩阵”

pre-public runtime 已经开始有：

- turn-local compaction tests
- post-compaction follow-up tests

但还缺一层更系统的 boundary matrix，例如：

- tool call group 不能被切断
- task result / wake hint / operator notification 在 compaction 前后保持因果一致
- different baseline budget / tail budget / exact-fit round 的矩阵

现在更像“有重要回归”，但还没成为一整套压缩边界体系。

### Waiting / External Trigger / NotifyOperator 还没形成专题测试组

这些路径其实已经在 `runtime_flow.rs` 和 `http_routes.rs` 里测了。

问题不是没测，而是：

- 还没被抽成一个明确的 waiting-plane regression suite

这会导致：

- 相关逻辑继续演进时，测试仍然分散
- 新人不容易快速看懂哪些测试在保护 waiting model

### Route / Delivery / Operator transport 仍然偏边界分散

remote operator transport、callback、generic webhook、public enqueue 这些都测了。

但现在它们分布在：

- HTTP route tests
- runtime flow tests
- 某些 types/storage/runtime unit tests

还缺一层“delivery / ingress / route semantics”专题测试。

### CI / Coverage 还比较粗

当前 `Makefile` 入口很简单：

- `make test -> cargo test`
- `make test-live -> cargo test live_ -- --nocapture`

这说明：

- 本地跑全量还行
- 但没有清晰的 test lane 划分
- coverage 也还没有自然进入日常反馈流

这和当前 open issue 里仍然保留：

- `#422 Post-refactor runtime test matrix`
- `#424 ci: align test coverage and add coverage reporting`

是吻合的。

## 3. 当前主要结构问题

我认为 pre-public runtime 现在最核心的测试结构问题有四个。

### 1. 两个超大集成文件承担过多职责

- `runtime_flow.rs`
- `http_routes.rs`

这是当前最明显的问题。

它们很有价值，但已经开始像“回归垃圾箱”：

- 新能力容易继续往里加
- 结果是覆盖很多，但组织性下降

### 2. Prompt tests 偏 section-level，不够 layout-level

pre-public runtime 已经有 prompt tests，但还缺：

- 对完整 emitted prompt 的 snapshot-style contract
- 对不同 turn/input surface 下 prompt 布局的对比测试

### 3. Benchmark 和源码测试之间缺清晰分工说明

现在 repo 和 workspace 里都已经有 benchmark 资产，但没有一份清晰约定：

- 哪类回归必须进 `cargo test`
- 哪类兼容性检查只进 live tests
- 哪类行为质量问题留给 benchmark

### 4. 测试命名和测试分层还没完全跟上 RFC 收口

现在 RFC 已经把一些概念收得更清楚了：

- `NotifyOperator`
- `External Trigger`
- `origin / delivery_surface / admission_context / authority_class`

但测试分层还没有完全按这些 contract 来重组。

## 4. 改进计划

## Phase 1：先重构测试结构，不先追求更多数量

目标：

- 不增加太多新 case
- 先把现有保护资产整理成更清楚的分层

建议动作：

1. 拆分 `runtime_flow.rs`
   - 按主题拆成：
   - `runtime_waiting_and_reactivation.rs`
   - `runtime_compaction.rs`
   - `runtime_tasks.rs`
   - `runtime_subagents.rs`
   - `runtime_workspace_worktree.rs`

2. 拆分 `http_routes.rs`
   - 按 surface 拆成：
   - `http_control.rs`
   - `http_events.rs`
   - `http_callback.rs`
   - `http_operator_transport.rs`
   - `http_workspace.rs`

3. 给测试文件名和 RFC contract 对齐
   - 不再继续往“总测试文件”里塞新 case

收益：

- 降低定位成本
- 让后续 coverage 和 CI 分 lane 更自然

## Phase 2：补 prompt/context snapshot contract

这是下一阶段最值得补的一层。

建议新增一组专门的 snapshot-style tests，覆盖：

- system prompt 全量布局
- context emitted section 顺序
- active work item / working memory / current input 的组合
- operator message / callback / system tick / task result 几种输入面的对比
- `origin / delivery_surface / admission_context / authority_class` 的提示词投影

目标：

- prompt 改动不再只靠 section 级断言
- 而是能直接看到 model-visible layout 的变化

这一层我建议优先借鉴 Codex 的思路。

## Phase 3：补 compaction boundary matrix

建议专门建立一组压缩边界测试，不再散落。

重点覆盖：

- exact-fit / over-budget / minimal-tail / baseline-over-budget
- tool result group 不被切断
- work item / waiting intent / wake hint / task result 在 compaction 后仍然语义稳定
- turn-local compaction 对 follow-up selection 的影响

目标：

- 把当前零散的 compaction regression 收成体系

这一层建议借鉴 Hermes 的 regression 风格。

## Phase 4：补 waiting-plane / delivery-plane 专题回归

建议把这几个路径收成专题测试组：

- waiting / wake / system tick
- callback / external trigger
- `NotifyOperator`
- operator ingress / transport delivery callback
- route resolution / reply route / target boundary

重点不是再测 HTTP shape，而是测：

- runtime semantics
- delivery semantics
- continuation semantics

这样做之后，remote operator transport 和 external trigger 后续演进会更稳。

## Phase 5：建立明确的 test matrix 和 CI lane

这一步对应现有的 `#422`、`#424`。

建议至少分成这些 lane：

1. `unit-fast`
   - 纯单元测试
   - 无 IO / 少量临时目录

2. `integration-runtime`
   - runtime / run_once / worktree / http 专题测试

3. `provider-contract`
   - provider parsing / request payload / auth / fallback

4. `live-smoke`
   - OpenAI / Codex / Anthropic
   - 手动或 nightly

5. `benchmark-live`
   - 不进常规 PR CI
   - 用于行为质量观测

同时建议：

- 接入 coverage 报告
- 至少给 `context`、`runtime::*`、`tool::*`、`http`、`tui` 做模块级 coverage 观察

## 5. 建议的优先顺序

如果只排近期最值得做的顺序，我建议：

1. **先做测试结构拆分**
   - 先拆 `runtime_flow.rs`
   - 再拆 `http_routes.rs`

2. **补 prompt/context snapshot contract**
   - 这是当前最缺的一层

3. **补 compaction boundary matrix**
   - 这是最容易出隐蔽回归的一层

4. **把 waiting/delivery 收成专题回归**

5. **最后再做 CI lane 和 coverage 对齐**

原因很简单：

- 如果不先拆结构，后面继续补 case，只会让两个大文件更重
- 如果不先补 prompt snapshot，agent 体验层回归仍然很难被机器发现

## 6. 对 issue 列表的建议映射

和当前 open issues 的关系，我建议这样看：

- `#422 Post-refactor runtime test matrix`
  - 对应本文的 Phase 5

- `#424 ci: align test coverage and add coverage reporting`
  - 也对应 Phase 5

另外我建议后续补几个新 issue：

1. `Split runtime_flow.rs into domain-focused integration suites`
2. `Split http_routes.rs into control/events/callback/operator suites`
3. `Add model-visible prompt/context snapshot tests`
4. `Add turn-local compaction boundary matrix`
5. `Add waiting and delivery semantics regression suite`

## 最后判断

pre-public runtime 当前测试现状可以概括成一句话：

> 已经有足够多的测试资产支撑下一阶段重构，但下一步最重要的工作不是再盲目加 case，而是把测试体系按 runtime contract 重新组织。

如果只给一个方向判断，那就是：

> 下一阶段把 **prompt contract** 和 **compaction/waiting regression** 提升到和现有 runtime/http 大集成测试同等重要的层级。
