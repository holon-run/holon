# Holon runtime 合并迁移方案（2026-04）

日期：`2026-04-29`

这份方案基于当前两个本地源码仓的实际结构，目标是把私有 runtime 实现线并回
`holon`，并让 `holon` 成为唯一公开产品边界。

这里不采用“公开两个名字再合并”的方式。私有 runtime 线没有公开发布过，
因此进入 `holon` 后不再保留旧产品名、旧二进制名、旧环境变量前缀或旧状态目录。

## 结论

采用 **clean import + Rust-first 直接替换 main**。

不做 git history merge，不把私有仓库改名后直接推成公开仓库，也不在公开叙事中解释
“两个项目如何合并”。更稳的做法是：

1. 先在私有 runtime 仓里完成全量改名和编译验证。
2. 验证通过后，把已经改名后的 Rust 工程按原目录结构复制到 `holon` repo root。
3. `holon` main 分支直接暴露 Rust 二进制命令。
4. 清理 Go 语言版本的 CLI/runtime 代码，不在 main 分支长期保留双实现。
5. 迁移完成后，再重新适配原来的 GitHub workflow / `solve` 入口。

允许 breaking change。当前 `holon` 已经发布过 Go 版本，需要旧行为的用户可以使用最新
Go release；`main` 分支不再承诺兼容旧 Go implementation 的 CLI、manifest、state 或
workflow contract。

对外只讲：

`holon` 是 local-first agent host and runtime。

## 源码现状

### `holon` 当前结构

本地路径：

`/Users/jolestar/opensource/src/github.com/holon-run/holon`

当前是 Go 主仓，核心边界是：

- `cmd/holon/`：公开 CLI 入口，包含 `run`、`solve`、`serve`、`message`、`tui`。
- `pkg/runtime/docker/`：现有 `holon run` 的 sandbox/container execution kernel。
- `pkg/serve/`：现有 experimental proactive runtime / subscription / JSON-RPC control plane。
- `pkg/tui/`：连接 `pkg/serve` JSON-RPC 的 Go TUI。
- `pkg/agenthome/`：现有 `agent_home` 模型。
- `pkg/skills/`、`skills/`、`pkg/builtin/`：技能发现、打包和内置资源。
- `agents/claude/`：Claude agent bundle。
- `holonbot/`、`.github/workflows/holon-solve.yml`：GitHub App / workflow 入口。

现有 `holon` 的公开资产已经成立，但这些资产不要求 main 分支继续兼容 Go 版本实现：

- CLI 名字是 `holon`。
- release/Homebrew/GitHub Actions 都围绕 `holon`。
- `holon solve` 已经是外部 GitHub workflow wrapper。
- 已发布 Go 版本可以继续作为旧用户的稳定 fallback。
- main 分支可以直接切到 Rust runtime，完成后再恢复/重建 GitHub workflow 入口。

### 私有 runtime 线当前结构

本地路径目前仍在私有源码仓，但进入公开前不保留旧项目名。

核心结构是 Rust crate：

- `Cargo.toml`：当前单 crate + 单 binary。
- `src/main.rs`：CLI surface，包含 `run`、`serve`、`daemon`、`config`、`prompt`、
  `status`、`tail`、`transcript`、`task`、`timer`、`control`、`agents`、
  `workspace`、`tui`、`debug`。
- `src/lib.rs`：runtime 模块聚合。
- `src/runtime/`：turn loop、task、continuation、closure、subagent、worktree、
  provider turn、message dispatch。
- `src/daemon/`：本地 daemon lifecycle。
- `src/provider/`：Anthropic-compatible、OpenAI Responses、Codex subscription
  transport、fallback/retry/diagnostics。
- `src/tool/`：model-facing tool surface，包括 shell、patch、task、memory、
  agent、workspace、sleep、operator notification。
- `src/system/`：host-local process/file/workspace abstraction。
- `src/memory/`：working memory、episode、search index。
- `src/tui/`：native terminal UI。
- `builtin_templates/`：agent templates。
- `benchmark/`、`benchmarks/`：benchmark harness、runner、task manifests。
- `docs/rfcs/`、`docs/implementation-decisions/`：大量 runtime contract 文档。

这条线已经具备可迁移的 runtime 能力：

- long-lived agent host
- explicit agent identity and profile
- queue / wake / sleep / continuation
- daemon lifecycle
- local control surface
- workspace binding and worktree projection
- task and child-agent execution
- provider abstraction and retry/fallback diagnostics
- local skills and agent-home guidance
- memory search/index
- token usage and attempt timeline
- native TUI

## 总体迁移原则

### 1. 只保留一个公开产品边界

公开仓库、README、CLI help、release note、workflow、Homebrew、docs 都只使用 `holon`。

旧名只允许在私有迁移脚本或本地过渡记录里出现，不进入公开 PR。

### 2. 不保留私有 history

私有 runtime 线是孵化过程资产，不是公开历史资产。

公开仓库里应出现的是整理后的产品代码：

- 保持已验证 Rust 工程目录结构
- 干净命名
- 干净 license
- 干净 README/CLI help
- 能通过公开 CI 的测试边界

### 3. main 分支允许 break

兼容旧 Go implementation 不是这次迁移的约束。

理由：

- 当前 `holon` 已经发布过版本，需要旧行为的用户可以固定到最新 Go release。
- 私有 runtime 线没有公开过，不需要对旧私有 CLI/state 提供公开兼容。
- 双实现并存会拖慢命名清洗和产品边界收束。
- `main` 分支的价值是尽快形成新的 `holon` runtime 主线，而不是长期维护迁移胶水。

### 4. Rust 直接成为 main 分支公开二进制

迁移后，main 分支上的 `holon` binary 应直接来自 Rust CLI。

Go 代码可以清理掉，保留范围只限于仍有必要的非 CLI 资产，例如：

- `holonbot/` 的 Node GitHub App 代码
- `agents/claude/` agent bundle 源码
- 仍可复用的 workflow / release 片段，后续再改成 Rust 版

不建议在 main 分支长期保留 Go launcher、Go runtime、Go serve 和 Rust runtime 的双轨结构。

## 目标目录结构

第一阶段不重构 Rust 目录结构。

私有 runtime 线当前已经是单 crate + 单 binary，`src/runtime`、`src/provider`、
`src/tool`、`src/system`、`src/memory`、`src/tui` 等模块关系已经能编译和测试。
迁移时应保持这套结构，避免把“改名、迁移、重构”混在一个 diff 里。

建议在 `holon` 中采用 root Rust app 结构：

```text
holon/
  Cargo.toml
  Cargo.lock
  Makefile
  src/
    main.rs
    lib.rs
    runtime/
    daemon/
    provider/
    tool/
    system/
    memory/
    prompt/
    tui/
  builtin_templates/
  benchmark/
  benchmarks/
  docs/
  agents/
  holonbot/
```

说明：

- root `Cargo.toml` 直接生成公开 `holon` binary。
- `src/` 保持私有 runtime 线当前模块布局。
- `docs/` 迁移仍然是当前 contract 的文档，archive 和孵化期讨论不整体搬。
- `benchmark/`、`benchmarks/` 迁移 benchmark harness，但先作为开发验证工具，不直接变成用户文档主入口。
- `cmd/holon/` 和 `pkg/` 不作为长期目录保留；迁移中如需临时对照，可以在迁移分支上保留，
  但最终 clean import PR 应清掉 Go CLI/runtime 实现。
- 如果后续确实需要拆分 crate 或引入 workspace，可以在迁移完成、CI 稳定后再做独立重构。

## 命名清洗规则

进入 `holon` 前必须完成以下替换。

### Cargo / binary

- package name：`holon`
- library name：`holon`
- public binary：`holon`

### 环境变量

旧前缀全部改为 `HOLON_`：

- runtime home：`HOLON_HOME`
- model default：`HOLON_MODEL`
- provider fallback：`HOLON_DISABLE_PROVIDER_FALLBACK`
- template GitHub API base：`HOLON_TEMPLATE_GITHUB_API_BASE`
- benchmark binary override：`HOLON_RUNTIME_BENCHMARK_BINARY`

### 状态目录

旧 runtime-owned hidden dir 改为：

```text
agent_home/
  .holon/
    state/
    ledger/
    indexes/
    cache/
```

worktree 临时目录改为：

```text
<tmp>/.holon-worktrees-<repo>
```

### 模板

模板 ID 改为：

- `holon-default`
- `holon-developer`
- `holon-reviewer`
- `holon-release`

模板目录同步改名。

### 文档与 UI 文案

所有用户可见文本改成 `Holon` / `holon`：

- CLI `about`
- TUI status
- daemon log hint
- prompt/debug output
- README
- runtime docs
- benchmark summaries
- model metadata

### Benchmark runner

runner 名称改为：

- `holon-runtime`
- `holon-openai`
- `holon-anthropic`
- `holon-codex`

benchmark artifact 中的旧路径、old runner id、old task id 不迁移为公开基线。
如果需要保留历史数据，只放在 workspace 私有研究材料里，不进入 `holon` repo。

## 迁移分期

### Phase 0：在私有 runtime 仓完成改名验证

目标：先在源仓把改名风险消化掉，确认 Rust 工程在新名字下仍能编译、测试和运行。

动作：

1. 在私有 runtime 仓创建临时迁移分支。
2. `Cargo.toml` package、lib、bin 全部改为 `holon`。
3. Rust import、CLI help、TUI 文案、provider metadata、template id、benchmark runner
   全部改成 `holon`。
4. hidden state dir 改为 `.holon`。
5. env var 改成 `HOLON_*`。
6. builtin templates 改成 `holon-*`。
7. runtime docs 中只保留能清洗成 Holon runtime contract 的文档。
8. 删除或暂不迁移不能自然清洗的历史讨论文档。

完成标准：

```bash
cargo fmt --check
cargo test
cargo build
cargo run -- --help
cargo run -- run "..." --json
rg -n "old-private-name|OLD_PRIVATE_PREFIX" src docs builtin_templates benchmark benchmarks
```

最后一条应为空。实际迁移时把命令里的占位符替换成旧名和旧环境变量前缀。

### Phase 1：复制已验证 Rust 工程到 `holon`

目标：把已经改名且验证过的 Rust 工程按原结构复制到 `holon` repo root。

动作：

1. 在 `holon` 创建迁移分支，例如 `runtime-rust-main`。
2. 不从私有仓库做 git subtree / git merge。
3. 清理或移出 Go CLI/runtime 主构建路径。
4. 复制已验证的：
   - `Cargo.toml`
   - `Cargo.lock`
   - `src/`
   - `builtin_templates/`
   - `docs/`
   - `benchmark/`
   - `benchmarks/`
   - 必要 scripts / Makefile
5. 不复制：
   - `.benchmark-results/`
   - `target/`
   - 本地 runtime state
   - archive docs
   - 私有 dogfood raw logs
6. 更新 root `.gitignore`：
   - `target/`
   - `.holon/`
   - `.benchmark-results/`
   - runtime benchmark temp files

完成标准：

- `cargo test`
- `cargo build`
- `cargo run -- --help` 显示公开 `holon` 命令。

### Phase 2：Rust CLI 直接成为 `holon`

目标：main 分支直接暴露 Rust 二进制命令，不再通过 Go launcher。

动作：

1. root `Cargo.toml` 的 binary name 设为 `holon`。
2. 公开命令直接来自 Rust CLI：
   - `holon run`
   - `holon serve`
   - `holon daemon`
   - `holon config`
   - `holon prompt`
   - `holon status`
   - `holon tail`
   - `holon transcript`
   - `holon task`
   - `holon timer`
   - `holon control`
   - `holon agents`
   - `holon workspace`
   - `holon tui`
   - `holon debug`
3. 更新 Makefile / release scripts，使 `make build` 或等价命令构建 Rust `holon`。
4. 移除或停用 Go CLI build path。

完成标准：

- `cargo run -- run "..." --json` 可用。
- `cargo run -- serve` 可用。
- `cargo run -- daemon status` 有明确输出。
- `cargo run -- tui --no-alt-screen` 可连接或给出明确失败原因。

### Phase 3：清理 Go CLI/runtime 实现

目标：避免 main 分支长期保留双实现。

动作：

1. 删除或移动出主构建路径：
   - `cmd/holon/`
   - `pkg/runtime/docker/`
   - `pkg/serve/`
   - `pkg/tui/`
   - 只服务旧 Go CLI 的 tests / schemas / docs
2. 保留仍有独立价值的非 Go-runtime 资产：
   - `agents/claude/`
   - `holonbot/`
   - `skills/`
   - 可复用的 docs / examples / workflow templates
3. 对 `pkg/agenthome`、`pkg/skills` 等 Go 包做取舍：
   - 如果 Rust runtime 已有等价实现，删除 Go 包。
   - 如果仍要复用概念，只迁移文档/fixture，不保留 Go implementation。

完成标准：

- root build 不再依赖 Go。
- Go runtime/serve/run/solve 的测试不再作为 main 分支 gate。
- README 不再描述 Go implementation。

### Phase 4：重新定义 `run` 和 `serve`

目标：接受 break 后，直接按新 runtime 语义定义 `holon run` / `holon serve`。

动作：

1. `holon run` 采用 Rust runtime 的 one-shot local execution 语义。
2. `holon serve` 采用 Rust runtime 的 long-lived host/control-plane 语义。
3. 不再追求旧 Go Docker runtime 的 manifest、mount、output dir 完全兼容。
4. 只保留必要的用户体验连续性：
   - `holon run "<goal>"`
   - `holon serve`
   - `HOLON_HOME`
   - workspace root / cwd flags

完成标准：

- 新 README 只描述 Rust runtime 语义。
- old Go release 是旧 contract 的兼容路径。
- main 分支不再出现临时 backend/preview flag。

### Phase 5：整理 benchmark 和 live-provider tooling

目标：在 clean import 已带入基础 harness 后，整理公开可复现的 benchmark 验证边界。

动作：

1. 保留已经清洗过的 benchmark harness。
2. 确认 runner 名称为 `holon-*`。
3. 确认 task id 已改成公开可读命名。
4. 不迁移 `.benchmark-results/`。
5. 重新生成公开可引用的 baseline。

完成标准：

- manifest validation 可运行。
- 至少一个 local fixture benchmark 可运行。
- live provider tests 默认 ignored/manual。

### Phase 6：重新适配 GitHub workflow / `solve`

目标：等 Rust runtime 成为 main 分支主线后，再恢复 GitHub workflow 能力。

动作：

1. 重新设计 `holon solve`，不要试图逐字段兼容旧 Go implementation。
2. 复用现有 GitHub workflow 的产品目标：
   - issue / PR context collection
   - branch / PR update
   - result publish
   - GitHub Actions invocation
3. 实现方式可以是 Rust CLI native，也可以先用独立 script/helper，但不回到 Go runtime。
4. 更新 `.github/workflows/holon-solve.yml` 和 examples。

完成标准：

- 新 `holon solve` 能完成一个 issue-to-PR smoke。
- GitHub workflow 使用 Rust `holon` binary。
- README 明确旧 Go workflow 只属于已发布旧版本。

## 文档迁移范围

### 应迁移

迁移并清洗为 Holon runtime docs：

- architecture overview
- agent profile / control plane
- workspace binding / execution roots
- execution policy
- instruction loading
- skill discovery
- external trigger / callback
- provider configuration
- memory model
- tool contract
- daemon lifecycle
- local operator troubleshooting

### 暂不迁移

不进入公开主仓：

- archive docs
- dogfooding raw logs
- benchmark raw result summaries
- 私有路径密集的 handoff notes
- 以旧项目名为主体的历史比较文档
- 未清洗的 roadmap/issue dump

这些材料可以继续留在 `workspace/projects/holon-run/` 作为内部资料。

## 测试迁移范围

第一批必须迁移：

- runtime unit tests
- tool schema tests
- provider request lowering tests
- daemon lifecycle tests
- run-once tests
- workspace/worktree tests
- memory index tests
- HTTP/control route tests

第二批再迁移：

- live provider tests
- benchmark harness
- real-repo benchmark manifests
- long-running dogfood scripts

测试命令目标：

```bash
cargo test
cargo build
```

后续可以加：

```bash
make test-runtime
make test
```

## Release 和 CI 调整

### CI

保留/新增 Rust job：

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets`
- `cargo test --workspace`

Node/非 runtime job 只保留仍然有价值、且不会阻塞 Rust 主线迁移的部分：

- agent bundle test/build
- workflow lint 在 GitHub workflow 重新适配前可以暂时停用或降级为手动检查
- release build 在 Rust release matrix 接上后恢复

Go job 不再作为 main 分支迁移 gate。需要旧 Go behavior 的用户走已发布版本。

### Release

main 分支 release 直接产出 Rust `holon` binary。

旧 Go release 不再从 main 分支重建。需要旧行为时，固定到迁移前最新 release。

release workflow 迁移顺序：

1. 先让本地 `cargo build --release` 产出 `target/release/holon`。
2. 再更新 GitHub release matrix。
3. 最后恢复 Homebrew formula / checksum 更新。

不要在 release tarball 中同时发布 Go `holon` 和 Rust `holon` 两个实现。

## Agent home 和状态边界

现有 `holon` 已有 `agent_home` 概念。新 runtime 应沿用这个用户可理解的模型，
但不需要兼容旧 Go implementation 的磁盘状态、manifest 或内部缓存布局。

建议目录：

```text
~/.holon/
  agents/
    <agent-id>/
      AGENTS.md
      CLAUDE.md
      state/
      cache/
      jobs/
      .holon/
        state/
        ledger/
        indexes/
        cache/
```

短期可以允许 runtime-owned `.holon/` 存在于 agent home 下，但文档要写清楚：

- `AGENTS.md` 是 agent 可读/可维护的身份文件。
- `.holon/` 是 runtime-owned，不应被 agent 当普通工作文件修改。
- `state/` 和 `.holon/state/` 的边界需要在迁移过程中继续收敛。

## 与其他层的边界

这次只并 runtime 线到 `holon`。

不顺手吞并：

- `AgentInbox`：继续作为 ingress / delivery layer。
- `uxc`：继续作为 capability layer。
- `webmcp-bridge`：继续作为 browser / web edge。

`holon` 应负责：

- local-first runtime
- agent host/control plane
- workspace and execution projection
- user-facing CLI/TUI/daemon surface
- high-level GitHub workflow wrapper

`holon` 不应负责：

- 所有 connector/source hosting
- 所有 protocol adapter
- 浏览器登录态桥接
- 重新做大模型平台

## PR 拆分建议

### PR 1：源仓改名验证

内容：

- 在私有 runtime 仓完成 package/lib/bin/env/template/docs/benchmark 全量改名
- 保持当前 Rust 目录结构不变
- 跑通编译和测试

验证：

- `cargo fmt --check`
- `cargo test`
- `cargo build`
- `cargo run -- --help`
- `rg` 确认旧名不再出现

### PR 2：Rust clean import 到 `holon`

内容：

- 复制已验证的 root Rust 工程到 `holon`
- 保持 `src/`、`docs/`、`benchmark/`、`benchmarks/`、`builtin_templates/` 结构
- 更新 `.gitignore` 和 build scripts

不包含：

- GitHub workflow 适配
- README 大改
- benchmark raw results / public baseline
- live-provider benchmark 调整

验证：

- `cargo test`
- `cargo build`
- `cargo run -- --help`

### PR 3：runtime docs baseline

内容：

- 只迁移当前有效 contract
- 删除旧项目名和孵化叙事
- 在主 README 加简短 runtime direction，但不重写全部文案

验证：

- `rg` 确认旧名不进入公开 docs
- markdown links 基本可用

### PR 4：Rust CLI becomes `holon`

内容：

- root Cargo project 直接产出 `holon`
- root Makefile / build scripts 切到 Cargo
- `holon run` / `holon serve` / `holon daemon` / `holon tui` 直接暴露
- 移除临时 backend/preview 入口

验证：

- `cargo run -- --help`
- `cargo run -- run "..." --json`
- daemon/status/tui smoke

### PR 5：Go implementation cleanup

内容：

- 删除 Go CLI/runtime/serve/tui 主实现
- 清理 Go tests 和旧 docs
- 保留 `agents/claude`、`holonbot` 等仍有价值的非 Go-runtime 资产

验证：

- root build 不依赖 Go
- Rust tests 通过
- README 不再描述 Go implementation

### PR 6：benchmark and live-provider tooling

内容：

- 整理 benchmark harness
- 确认 runner 改名
- 确认 task ids 改名
- provider live tests 放到 ignored/manual path

验证：

- manifest validation
- one local fixture benchmark
- manual live provider command documented

### PR 7：GitHub workflow / solve re-adaptation

内容：

- 重新实现或适配 `holon solve`
- 更新 `.github/workflows/holon-solve.yml`
- 更新 examples 和 docs

验证：

- issue-to-PR smoke
- workflow dispatch smoke
- publish/update behavior smoke

## 风险和处理

### 风险 1：一次性改名太大

处理：

- 先在私有迁移分支完成 mechanical rename。
- 公开 PR 只展示清洗后的代码。
- 不让 review 过程夹杂旧名讨论。

### 风险 2：main 分支 break 影响旧用户

处理：

- README 明确旧 Go behavior 请使用迁移前最新 release。
- release tag / changelog 明确 main 分支进入 Rust runtime line。
- 不在 main 上维护双实现兼容层。

### 风险 3：安全模型倒退

处理：

- Rust runtime 初期不宣称强 sandbox。
- 文档明确 host-local execution profile 的真实边界。
- 如果后续需要强隔离，再单独设计 execution backend，不拿旧 Go Docker path 当兼容包袱。

### 风险 4：状态目录和 agent_home 混乱

处理：

- 所有新 runtime 状态统一放在 `.holon/`。
- 旧私有 runtime state 不做公开兼容承诺。
- 如果本地已有私有 dogfood 数据，需要写一次性私有迁移脚本，不进入公开 release。

### 风险 5：benchmark 历史污染公开叙事

处理：

- 不迁移 `.benchmark-results/`。
- 只迁移 harness 和少量干净 task manifest。
- 公开 benchmark 从 `holon-*` runner 重新生成。

## 立即下一步

1. 在 `holon` 创建迁移分支。
2. 先在私有 runtime 仓创建改名分支。
3. 保持目录结构不变，完成全量命名清洗。
4. 在源仓跑 `cargo fmt --check`、`cargo test`、`cargo build`、`cargo run -- --help`。
5. 验证通过后再复制到 `holon` repo root。
6. 切 root build，使 main 分支直接构建 Rust `holon`。
7. 清理 Go CLI/runtime 实现。
8. 迁移完成后再适配 GitHub workflow / `solve`。

这条路线的关键不是“把一个项目公开并合并”，而是让 `holon` 直接长出下一代
runtime。私有实现线只作为源码来源和设计证据存在，不作为公开品牌或公开历史存在。
