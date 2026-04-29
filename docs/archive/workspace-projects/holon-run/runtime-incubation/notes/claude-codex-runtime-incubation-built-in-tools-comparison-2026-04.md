# Claude Code、Codex 与 pre-public runtime 内置 Tools 比较

这篇文档不是泛泛比较三个产品，而是只比较它们的内置 tool contract。

关注点有三个：

1. Claude Code 和 Codex 的内置 tools，到底分别代表什么设计思想。
2. pre-public runtime 当前把两家的东西拼在一起后，哪里已经融合得不错，哪里还不一致。
3. 对 pre-public runtime 来说，下一步应该往哪边收敛，哪些该借鉴，哪些不该抄。

## 参考基线

pre-public runtime 侧基线：

- `runtime-incubation/docs/tool-contract-audit.md`
- `runtime-incubation/docs/basic-tool-comparison.md`
- `runtime-incubation/src/tool/dispatch.rs`
- `runtime-incubation/src/tool/execute.rs`

Claude Code 侧一手参考：

- `claude-code-source-code/src/Tool.ts`
- `claude-code-source-code/src/constants/prompts.ts`
- `claude-code-source-code/src/constants/tools.ts`
- `claude-code-source-code/src/tools/FileReadTool/*`
- `claude-code-source-code/src/tools/FileEditTool/*`
- `claude-code-source-code/src/tools/FileWriteTool/*`
- `claude-code-source-code/src/tools/GrepTool/prompt.ts`
- `claude-code-source-code/src/tools/BashTool/prompt.ts`

Codex 侧一手参考：

- `openai/codex/codex-rs/tools/src/tool_registry_plan.rs`
- `openai/codex/codex-rs/tools/src/tool_config.rs`
- `openai/codex/codex-rs/tools/src/local_tool.rs`
- `openai/codex/codex-rs/tools/src/agent_tool.rs`
- `openai/codex/codex-rs/tools/src/request_user_input_tool.rs`
- `openai/codex/codex-rs/tools/src/apply_patch_tool.rs`

## 先说结论

如果只看内置 tools 的设计哲学：

- Claude Code 更像“面向模型工作流的产品级工具面”。
- Codex 更像“面向运行时能力编排的工具平面”。
- pre-public runtime 目前处在两者之间：公共 coding surface 借了 Claude，runtime primitive 和 shell / patch 借了 Codex，但还没有把“哪一层应该像谁”讲清楚，也还没有把 contract 做到同一成熟度。

对 pre-public runtime 最合适的方向，不是二选一，而是：

- 公共 coding tools 继续保持 Claude 风格的强模型先验。
- tool registry、feature gating、handler layering、structured output 继续朝 Codex 靠。
- pre-public runtime 自己的 runtime tools 保持 pre-public runtime 语义，但要统一命名、输入校验、输出 envelope、prompt guidance 和测试形状。

## 1. Claude Code 的内置 tools 在表达什么

Claude Code 的 tools，本质上不是“运行时最低层 primitive”，而是“模型最容易理解并稳定使用的工作语义”。

最典型的一组就是：

- `Read`
- `Write`
- `Edit`
- `Glob`
- `Grep`
- `Bash`

这些工具不是随便起名的，而是被当成一套工作流语言来设计的。

例如：

- `Read` 的 prompt 直接规定绝对路径、默认读取范围、分页读取 PDF、截图阅读、notebook 阅读等行为。
- `Edit` 的 prompt 明确要求“先 Read 再 Edit”，并把 exact replacement 的失败模式提前教给模型。
- `Write` 的 prompt 明确说它主要用于新文件或整文件重写，而不是一般修改。
- `Grep` 的 prompt 明确禁止把搜索退化成 `rg` 或 `grep` 的 shell 调用。
- `Bash` 的 prompt 不只是参数说明，而是一整套 shell、git、sandbox、background task、commit/PR 使用规范。

这说明 Claude Code 的工具设计重点不是“schema 精确就够了”，而是：

- 让模型知道什么时候用
- 让模型知道什么时候不要用
- 让模型形成稳定的多步操作顺序

### Claude Code 的一致性是怎么表达出来的

Claude Code 的一致性主要靠四层叠加表达：

1. `buildTool(...)` 这一套统一骨架。
2. 每个 tool 自己的 `prompt()`。
3. 每个 tool 的 `inputSchema` / `outputSchema` / permission check / render hooks。
4. `constants/tools.ts` 和 `constants/prompts.ts` 里按 session / agent / mode 再做二次约束。

也就是说，Claude Code 的“tool contract”不是单个 JSON schema，而是：

- 名字
- prompt
- schema
- 权限规则
- mode 下的 allow / disallow
- transcript / UI 表达

一起组成的。

这是 Claude Code 非常强的一点。模型看到的不是一堆孤立函数，而是一套被 prompt 深度解释过的工作语言。

### Claude Code 的优点和代价

优点：

- 文件和搜索工具的模型先验非常强。
- `Read -> Edit/Write` 这种基本顺序非常稳定。
- prompt 对误用有大量事先约束。
- tool 不只是 capability，也是 workflow hint。

代价：

- tool surface 很容易产品化、膨胀化。
- 同一个 runtime 里会出现很多“产品特性型工具”，比如 plan、task、team、cron、web、worktree、skill、tool search。
- 一致性主要靠 prompt 和大量特殊规则维持，runtime 边界本身未必始终最简洁。

所以 Claude Code 的 tool 体系更像“一个成熟产品的 agent DSL”，而不是一个最小、纯粹的 headless runtime substrate。

## 2. Codex 的内置 tools 在表达什么

Codex 的 built-in tools，重点不在“给模型一套 workflow 话术”，而在“给运行时一个清晰的能力装配平面”。

从 `tool_config.rs` 和 `tool_registry_plan.rs` 可以看得很清楚：

- 先有 `ToolsConfig`
- 再根据 feature、model info、session source、sandbox policy、environment 计算真实可用 surface
- 最后由 `build_tool_registry_plan()` 统一注册 tool spec 和 handler

Codex 的 tool 设计首先回答的是：

- 当前环境有没有 shell
- shell 用哪种后端
- 是否允许 unified exec
- 是否暴露 MCP resource tools
- 是否开启 collaboration tools
- 是否启用 `request_user_input`
- 是否暴露 `apply_patch`
- 是否启用 web/image/search 等工具

这是一种非常 runtime-oriented 的思路。

### Codex 的一致性是怎么表达出来的

Codex 的一致性，主要不是靠每个 tool 的长 prompt，而是靠下面这些机制：

1. 统一的 `ToolSpec` / `ResponsesApiTool` / freeform tool 表达。
2. 中央 registry plan 统一注册 spec 和 handler。
3. `ToolsConfig` 统一控制 feature gating、mode gating、environment gating。
4. 工具族明确分层：
   - local exec / shell
   - patch
   - plan
   - request user input
   - MCP resources
   - view image / web search / image generation
   - collaboration / multi-agent
   - dynamic / namespaced MCP tools

Codex 的一致性是“工具平面的一致性”，不是“模型工作流语言的一致性”。

例如：

- `exec_command` 和 `write_stdin` 不是两个随意的工具，而是一套 unified exec 会话协议。
- `spawn_agent`、`send_message`、`followup_task`、`wait_agent`、`close_agent` 是一整套 collaboration protocol。
- 动态 MCP tools 支持 namespaced tool name，本质上是在把外部能力纳入同一 registry。

这也是为什么 Codex 的 tool surface 看起来比 Claude Code 更“系统编程”一些。

### Codex 的优点和代价

优点：

- runtime boundary 清晰。
- feature / mode / environment 驱动的暴露逻辑统一。
- tool family 之间更像 protocol，不像一堆孤立按钮。
- output schema 和 handler mapping 更容易系统化。

代价：

- 对模型来说，很多工具不是天然强先验，需要系统提示和上层纪律去教。
- 如果直接拿 Codex 那套思路做 coding agent 的公共 surface，文件搜索和修改类工具未必像 Claude Code 那样顺手。
- namespaced / dynamic tool plane 很强，但对 pre-public runtime 这种还在收紧核心 runtime 的系统来说，容易过重。

## 3. pre-public runtime 当前融合了什么

pre-public runtime 当前其实已经形成了一个很清楚的混合体。

从 public surface 看，已经能分出三块：

### 3.1 Claude 风格的 coding tools

pre-public runtime 现在公开了：

- `Glob`
- `Grep`
- `Read`
- `Write`
- `Edit`

这组明显是在借 Claude Code 的 naming 和模型习惯。

这是对的。因为这几个名字本身就是强先验，能减少 prompt friction。

### 3.2 Codex 风格的 shell / patch tools

pre-public runtime 同时用了：

- `ApplyPatch`
- `exec_command`

这也是合理的，因为：

- `ApplyPatch` 的 patch envelope 比单纯 exact replacement 更稳。
- `exec_command` 这套参数形状更适合受控 shell 和后续扩展。

### 3.3 pre-public runtime 自己的 runtime coordination tools

pre-public runtime 还有一组明显不是 Claude 也不是 Codex 直接原样照搬的工具：

- `Sleep`
- `GetAgentState`
- `Enqueue`
- `CreateTask`
- `TaskList`
- `TaskGet`
- `TaskOutput`
- `TaskStop`
- `CreateCallback`
- `CancelWaiting`
- `EnterWorkspace`
- `ExitWorkspace`
- `WorktreeTaskDiscard`
- `update_work_item`
- `update_work_plan`

这组才是 pre-public runtime 真正的 runtime surface。

它们体现的是 pre-public runtime 自己关心的东西：

- 单 session runtime
- background task lifecycle
- waiting / callback
- workspace projection
- work item / work plan

这部分不是问题。问题在于这组 tool 还没有形成像 Claude 或 Codex 那样清晰的一致性表达。

## 4. pre-public runtime 现在不一致的地方

`docs/tool-contract-audit.md` 的核心判断我认同，而且放到当前代码上看依然成立：pre-public runtime 的问题不是“tool 不够多”，而是“surface 不够齐”。

不过要补一个当前状态说明：

- `tool-contract-audit.md` 写作时，`Glob`、`Grep`、`Read` 还处于半退役 / 未公开的尴尬状态。
- 当前代码里，这三个工具已经重新进入 public surface。

所以审计文档里“retired discovery tools”的部分已经部分过时，但它对 pre-public runtime 不一致性的判断并没有过时。

### 4.1 命名层级不一致

pre-public runtime 当前的工具命名混了四种风格：

- Claude 风格 PascalCase 名词动词：`Glob` `Grep` `Read` `Write` `Edit`
- Codex 风格：`exec_command`
- Codex patch 风格但大写：`ApplyPatch`
- pre-public runtime 自己的 PascalCase runtime verbs：`CreateTask` `TaskList` `TaskStop`
- 唯一一组 snake_case pre-public runtime-native tools：`update_work_item` `update_work_plan`

这意味着当前并没有一个清楚的公开规则告诉模型：

- 什么是 borrowed prior
- 什么是 pre-public runtime-native primitive
- 为什么有的 tool 是 PascalCase，有的是 snake_case

其中最突兀的是：

- `update_work_item`
- `update_work_plan`

因为它们既不是 Claude 先验名字，也不是 Codex 既有 public prior，却成了 pre-public runtime-native surface 里唯一的 snake_case 例外。

### 4.2 抽象层级不一致

pre-public runtime 当前把不同抽象层的东西同时暴露给模型：

- coding primitive：`Read` `Edit` `ApplyPatch`
- runtime inspection：`GetAgentState` `TaskList` `TaskGet`
- runtime mutation：`CreateTask` `TaskStop`
- external orchestration：`CreateCallback` `CancelWaiting`
- environment switching：`EnterWorkspace` `ExitWorkspace`
- durable planning state：`update_work_item` `update_work_plan`

这本身不是错误。错误是这些层之间还没有一套统一的 family contract。

Claude Code 会在 prompt 里告诉模型每一类工具怎么配合使用。
Codex 会在 registry / handler / config 层把每一类工具当成 protocol 管。

pre-public runtime 现在这两层都还不够完整。

### 4.3 输出 contract 不一致

这是 `tool-contract-audit.md` 里最重要的一点之一。

pre-public runtime 当前有三种输出风格并存：

- 结构化 JSON：`TaskList` `TaskGet` `TaskOutput` `update_work_item` `update_work_plan`
- pretty JSON 串：部分 inspection / callback tools
- 纯文本 receipt：`CreateTask` `TaskStop` `Write` `Edit` `ApplyPatch` `exec_command`

这会直接导致：

- tool chaining 不稳定
- prompt 里很难给出统一规则
- 上下文压缩时很难抽取标准化事实

对 pre-public runtime 来说，这比命名问题更本质。因为 pre-public runtime 是长生命周期 agent，tool 输出不是只给当前一轮模型看的，也会进入后续 compact / summary / memory。

### 4.4 输入校验形状不一致

pre-public runtime 当前有两种世界并存：

- 一部分工具已经走 typed args + semantic validation
- 一部分工具仍然在 `execute.rs` 里直接从 `serde_json::Value` 手工取字段

这会带来三层漂移：

- schema 说的是一套
- parser 真正接受的是一套
- runtime 实际语义又是另一套

这在 `CreateTask` 和 `exec_command` 上尤其明显，也是审计文档重点点出来的问题。

### 4.5 prompt guidance 覆盖不一致

pre-public runtime 在高频工具上已经有了一部分不错的 prompt guidance，但覆盖还不均匀。

当前最成熟的仍然是：

- `ApplyPatch`
- `Edit`
- `exec_command`
- task inspection family 的部分规则

但在 pre-public runtime 自己最有特色的 runtime tools 上，反而没有形成 Claude Code 那样强的 tool usage language，例如：

- 什么时候应该先 `TaskList` 再 `TaskGet`
- 什么时候该 `TaskOutput`
- 什么时候该 `CreateTask`
- `CreateCallback` / `CancelWaiting` 与 waiting state 的配合原则
- `EnterWorkspace` / `ExitWorkspace` 的 destructive boundary

这导致 pre-public runtime 自己最有特色的一组 tools，模型先验反而最弱。

## 5. 对 pre-public runtime 最合适的改进方向

我的判断是：pre-public runtime 不应该试图“全面 Claude 化”或“全面 Codex 化”。

更合适的是一个三层策略。

## 5.1 第一层：公共 coding surface 保持 Claude 风格

这一层建议继续保留：

- `Glob`
- `Grep`
- `Read`
- `Write`
- `Edit`

原因很简单：

- 这是最强的 coding-agent 公共先验之一。
- 模型更容易稳定使用。
- 这些工具本身属于用户意图最直接的一层。

这里不建议为了“统一命名风格”去改成别的名字。

`ApplyPatch` 和 `exec_command` 也可以继续保留现名，因为这两个名字已经有较强 Codex 使用先验，而且能力边界明确。

换句话说，pre-public runtime 在 coding surface 上不应该为了内部整洁牺牲模型先验。

## 5.2 第二层：runtime coordination tools 走 pre-public runtime-native 统一规则

这一层建议明确声明为 pre-public runtime 自己的 runtime tool family，包括：

- `Sleep`
- `GetAgentState`
- `Enqueue`
- `CreateTask`
- `TaskList`
- `TaskGet`
- `TaskOutput`
- `TaskStop`
- `CreateCallback`
- `CancelWaiting`
- `EnterWorkspace`
- `ExitWorkspace`
- `WorktreeTaskDiscard`
- `UpdateWorkItem`
- `UpdateWorkPlan`

这里的关键不是一定要跟 Claude 或 Codex 同名，而是要统一 pre-public runtime 自己的 contract 表达。

我建议：

- pre-public runtime-native public tools 一律用 PascalCase。
- 因此把 `update_work_item` 改成 `UpdateWorkItem`。
- 把 `update_work_plan` 改成 `UpdateWorkPlan`。

这样可以把公共 surface 的命名规则讲清楚：

- borrowed coding priors：保留 upstream 常用名字
- pre-public runtime-native runtime tools：统一 PascalCase

这比“所有工具强行一种 casing”更现实，也更容易让模型理解。

## 5.3 第三层：registry、handler、feature gating 朝 Codex 靠

pre-public runtime 最该借 Codex 的，不是 namespaced MCP tool 大平面本身，而是它的工具装配纪律：

- registry centralization
- spec / handler separation
- config-driven exposure
- environment-aware gating
- family-based registration

这部分 pre-public runtime 其实已经开始做了，但还不够彻底。

下一步最值得做的是：

1. 把 public tool family 正式分层写进文档和代码注释。
2. 把 schema、parser、semantic validation 聚到每个 tool 自己附近。
3. 把返回值统一成 structured-first envelope。
4. 把 prompt guidance 也按 tool family 组织，而不是只零散给个别工具。

## 6. 具体取舍建议

下面是我认为 pre-public runtime 应该明确做出的取舍。

### 6.1 应该继续借鉴 Claude Code 的点

- 文件和搜索工具命名。
- `Read -> Edit/Write` 这类强工作流提示。
- 不鼓励把搜索退化到 shell。
- tool-specific prompt guidance。
- 针对模型误用的预防式说明。

### 6.2 应该继续借鉴 Codex 的点

- central registry + handler registration
- tool family 分层
- feature / mode / environment 驱动的暴露逻辑
- collaboration / async protocol 的协议化思路
- `exec_command` / `write_stdin` 这种“不是一次调用，而是一个小协议”的设计意识
- structured output schema 优先

### 6.3 不建议现在复制 Claude Code 的点

- 大量产品特性型工具一起进核心 runtime
- 过重的 task/team/plan/tool-search/tool-skill 工具森林
- 太依赖 prompt 大段说明去弥补 runtime contract 不清楚

pre-public runtime 当前最需要的是把已有工具磨齐，不是快速扩 surface。

### 6.4 不建议现在复制 Codex 的点

- 全量 namespaced dynamic tools 体系
- 为多种前端 / mode / product shape 做过早平台化
- 在 public surface 上过度暴露底层 runtime primitive

pre-public runtime 现在还是 headless single-session runtime 优先，不需要为了未来可能的 MCP / app ecosystem 提前把 tool plane 做得很重。

## 7. 一个更清楚的 pre-public runtime 工具策略

我建议 pre-public runtime 未来明确把工具策略写成下面这样。

### 7.1 Coding tools

目标：服务代码探索、读取、修改、验证。

工具：

- `Glob`
- `Grep`
- `Read`
- `Write`
- `Edit`
- `ApplyPatch`
- `exec_command`

策略：

- 强调模型先验和使用顺序。
- 默认响应尽量紧凑。
- `exec_command` 返回结构化 envelope，而不是纯文本。

### 7.2 Runtime coordination tools

目标：服务长生命周期 session 的任务、等待、工作计划和回调。

工具：

- `Sleep`
- `GetAgentState`
- `Enqueue`
- `CreateTask`
- `TaskList`
- `TaskGet`
- `TaskOutput`
- `TaskStop`
- `CreateCallback`
- `CancelWaiting`
- `UpdateWorkItem`
- `UpdateWorkPlan`

策略：

- 全部使用统一命名规则。
- 全部使用 typed parsing + semantic validation。
- inspection 和 mutation 分开建模。
- 统一返回 structured receipt。

### 7.3 Workspace boundary tools

目标：服务 execution projection / worktree / destructive boundary。

工具：

- `EnterWorkspace`
- `ExitWorkspace`
- `WorktreeTaskDiscard`

策略：

- 强化 destructive guidance。
- 返回结构化 boundary receipt。
- 明确 keep / discard / projection 这些字段的 runtime effect。

## 8. 我对 pre-public runtime 的最终建议

如果只允许给一个方向判断，我的建议是：

pre-public runtime 应该把自己定义成：

- Claude 风格的 coding surface
- Codex 风格的 tool substrate discipline
- pre-public runtime 自己的 runtime coordination language

换句话说：

- Claude 提供“模型怎么自然地做代码工作”的外层语言。
- Codex 提供“runtime 怎么把工具平面组织清楚”的内层纪律。
- pre-public runtime 自己负责“长生命周期 headless session 需要哪些原生 runtime tools”。

当前 pre-public runtime 最大的问题不是借得太杂，而是还没有把这个三层关系明确写出来，并落实到：

- naming
- schema
- parsing
- output
- prompt guidance
- tests

一旦这六件事按 family 收敛，pre-public runtime 的 tool surface 会比现在稳定很多，而且不需要继续盲目加工具。
