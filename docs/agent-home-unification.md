# Agent Home Unified Model (Design + Refactor Plan)

## 1. 背景

当前 `holon run`、`holon solve`、`holon serve` 在运行模型上有分裂：

1. `run/solve` 更接近一次性任务模型（input/workspace/output/state）。
2. `serve` 是常驻控制面模型（事件循环、session 持续、记忆持续）。
3. 提示词来源和角色配置在不同命令中缺少统一抽象。

随着项目从“一次性执行”演进到“长期自治协作”，需要把三者统一到同一套 Agent 抽象上。

## 2. 目标

1. 用统一的 `agent_id + agent_home` 抽象覆盖 `run/solve/serve`。
2. 支持 Agent 长期演进（角色、身份、风格、记忆、session）并可持久化。
3. 允许 `run/solve` 在未指定参数时走临时 Agent（保持轻量使用体验）。
4. 明确提示词分层：系统契约不可修改，Agent persona 文件可演进。
5. 把命令参数简化为“指定 Agent”，而不是在每个命令重复大量 flags。

## 3. 关键决策

### 3.1 命名

对外命名采用 `agent` 语义：

1. `instance_id` -> `agent_id`
2. `instance_home` -> `agent_home`

内部实现可以保留 instance 结构体名，但 CLI/API/文档统一暴露 `agent_*`。

### 3.2 agent_home 推断

`agent_home` 默认通过 `agent_id` 推断：

1. 默认根目录：`~/.holon/agents/<agent_id>/`
2. 解析优先级：`--agent-home` > `--agent-id` 推断 > 命令默认

### 3.3 agent_id 默认

`agent_id` 需要默认值（对用户可省略，对系统必须存在）：

1. `serve` 默认 `agent_id=main`
2. `run/solve` 若未指定 `agent_home/agent_id`，默认创建临时 agent（一次性）
3. `agent_id` 只允许安全字符：`[a-zA-Z0-9_-]+`

### 3.4 提示词分层与可变性

借鉴 openclaw 的“系统契约 + workspace 注入”模式，Holon 采用以下边界：

1. 不可变层（系统内置，agent 不可改）
- system contract（run/serve 各自 overlay）
- tool policy/runtime contract

2. 可演进层（位于 `agent_home`，agent 可改）
- `AGENT.md`
- `ROLE.md`
- `IDENTITY.md`
- `SOUL.md`

3. 运行时组装
- 每轮重建 system prompt：
  1) immutable contract
  2) mode overlay (`run` / `serve`)
  3) runtime/tool/event context
  4) agent-home persona files

### 3.5 workspace 抽象（与 agent_home 解耦）

`agent_home` 与 `workspace` 分属两层：

1. `agent_home` = 身份与记忆面
- 持久化配置、persona、memory、session、cursor、日志
- 不承载“当前要修改的业务代码目录”语义

2. `workspace` = Job 执行面
- 每个 Job 绑定一个 `WorkspaceRef`
- 可指向本地目录、repo worktree、临时 clone 等
- 一个 Agent 可在生命周期中操作多个 repo/workspace

统一原则：

1. `agent_home` 回答“我是谁”
2. `workspace` 回答“我现在在改什么”

## 4. 统一运行模型

### 4.1 抽象层次

1. Agent（长期身份）
- 对应 `agent_home`
- 包含配置、persona、状态、记忆、sessions

2. Job（一次执行）
- 每个事件或任务触发一个 job
- 有 job 级 input/workspace/output
- workspace 由 JobContext 绑定，不默认等同于 `agent_home/workspace`

统一解释：

1. `serve` = 持久 Agent + 多个连续 Job
2. `run` = 临时 Agent + 单 Job（除非显式指定 `agent_home`）
3. `solve` = 临时或持久 Agent + 单 Job（可按 link 选择专业默认 profile）

### 4.2 agent_home 目录建议

```text
~/.holon/agents/<agent_id>/
  agent.yaml
  AGENT.md
  ROLE.md
  IDENTITY.md
  SOUL.md
  state/
    serve-state.json
    goal-state.json
    memory.md
  sessions/
    session-index.json
    *.jsonl
  channels/
    event-channel.ndjson
    event-channel.cursor
  jobs/
    <job-id>/
      input/
      output/
```

说明：

1. `agent_home` 不要求包含固定 `workspace/` 目录。
2. workspace 由每个 Job 单独决定，可来自外部路径或前置准备流程。

## 5. 命令语义统一

### 5.1 `holon serve`

目标：默认进入持久 Agent 模式。

1. 默认：`--agent-id main` -> `~/.holon/agents/main`
2. 支持 `--agent-id` 与 `--agent-home` 覆盖
3. 启动时从 `agent_home` 加载配置、persona、state、session
4. 不再要求大量 `--state-dir`、`--controller-workspace` 组合（可保留兼容映射）
5. 不绑定单一 workspace；每个事件生成 Job 时解析自己的 `WorkspaceRef`

### 5.2 `holon run`

目标：默认轻量一次性，显式可持久。

1. 未指定 `--agent-id/--agent-home`：创建临时 `agent_home`（任务结束可清理）
2. 指定 `--agent-id`：进入持久 Agent 运行
3. 默认 persona 使用内置模板（当 `agent_home` 无文件时）
4. 支持参数覆盖 persona/profile
5. `run` 直接操作传入的 workspace，不做默认复制
6. 若需要隔离副本（如 local clone/worktree），由调用方在前置步骤准备 workspace

### 5.3 `holon solve`

目标：默认走“专业 profile + 一次性 agent”，可升级为持久 agent。

1. 未指定 `agent_*`：创建临时 `agent_home`
2. 根据输入 link 类型加载内置专业 profile
- 当前优先支持 GitHub issue/pr 场景
3. 支持 `--agent-id/--agent-home` 复用长期 agent
4. 可在执行前按策略准备 workspace（如 worktree/clone），再交给统一执行面

## 6. 配置模型

新增 `agent.yaml`（最小草案）：

```yaml
version: v1
agent:
  id: main
  mode: serve # serve | run | solve
  profile: default # default | github-issue | github-pr-review | ...
runtime:
  workspace:
    mode: direct # direct | prepared
    path: .
    prepare: none # none | worktree | clone
  cleanup: auto
prompt:
  persona_files:
    - AGENT.md
    - ROLE.md
    - IDENTITY.md
    - SOUL.md
  system_contract: builtin://contracts/serve
event_source:
  type: github-webhook
  repo: owner/repo
```

约束：

1. `system_contract` 只能引用内置 contract（不可写路径）。
2. persona_files 可缺省，缺失时使用内置默认内容或空内容。
3. `runtime.workspace.mode=direct` 表示直接操作 `path`，不复制。
4. `runtime.workspace.mode=prepared` 表示先执行准备动作（如 worktree/clone）再运行 Job。

## 7. 重构计划（分阶段）

### Phase 1: 抽象与路径收敛

1. 新增 `pkg/agenthome`（或等价模块）
- 解析 `agent_id/agent_home`
- 创建目录、读取 `agent.yaml`
- 提供默认路径与安全校验

2. 在 `cmd/holon` 引入统一参数
- `--agent-id`
- `--agent-home`

3. 把现有 `serve` 的 `state-dir/controller-workspace` 映射到 `agent_home`
- 先兼容，后逐步弃用旧 flags

### Phase 2: Prompt Loader 统一

1. 新增统一 prompt 组装器
- 输入：mode + runtime + tools + persona files
- 输出：system/user prompt

2. 将 `run/solve/serve` 改为共用同一组装器

3. 引入 immutable contract 资产目录
- `contracts/common.md`
- `contracts/run.md`
- `contracts/serve.md`

### Phase 3: Serve 持久化模型迁移

1. 将 `controller-state/*`、event channel、session 元数据迁移到 `agent_home/state|channels|sessions`
2. 引入 `agent.lock` 防止同一 `agent_home` 多进程并发写
3. 统一日志与 job 输出位置到 `agent_home/jobs/<job-id>/output`
4. 为 serve 增加 `WorkspaceRef` 解析器（事件 -> repo/ref/path -> workspace）

### Phase 4: Run/Solve profile 化

1. `run` 默认 profile = `default`
2. `solve` 根据 link 自动选择 profile（issue/pr）
3. profile 仅决定默认提示词与默认 skills，不改变底层执行模型
4. `solve` 提供标准 workspace 准备器（none/worktree/clone）

### Phase 5: 兼容与清理

1. 文档切换到 `agent_id/agent_home` 术语
2. 旧参数打印迁移告警并映射到新模型
3. 移除不再需要的 controller 专用参数

## 8. 测试计划

1. 单元测试
- `agent_id -> agent_home` 推断
- 参数优先级解析
- `agent_id` 安全字符校验
- prompt 分层加载顺序

2. 集成测试
- `serve` 默认 `main` agent 启动与重启恢复
- `run` 临时 agent 生命周期
- `solve` link -> profile 自动选择
- 同一 `agent_home` 锁冲突检测

3. 回归测试
- 旧参数兼容路径（至少一版过渡）
- 现有 webhook/JSONL 行为不回退

## 9. 迁移策略

1. 文档先行：新增本设计文档与 runbook。
2. 代码先引入新参数和新目录，不立即删除旧参数。
3. 稳定后再切换默认文档示例到 `--agent-id/--agent-home`。

## 10. 开放问题

1. `run` 的临时 `agent_home` 默认清理策略是否保留最近 N 个用于调试。
2. `solve` 的 profile 映射是否仅基于 link 类型，还是允许仓库级规则覆盖。
3. persona 文件自动演进是否需要版本快照（例如 `state/prompt-history/`）。
4. 是否提供 `holon agent init` 命令一次性生成 `agent_home` 标准骨架。
5. serve 的 workspace 缓存策略是否需要（例如复用既有 worktree 以加速）。

---

该文档作为 `agent_home` 统一改造的基线设计，后续 RFC/实现以此为准迭代。
