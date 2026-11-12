---
title: 搭建持续同步代码与文档的本地 Docs Steward
summary: 从 docs-steward 模板创建文档维护 Agent，通过对话让它挂载代码仓库、审计代码与文档的不一致漂移，并在独立 Git worktree 中提交带有证据引用的纯文档 PR。
order: 87
---

# 搭建持续同步代码与文档的本地 Docs Steward

本指南演示如何基于 `docs-steward` 模板搭建一个常驻的文档维护 Agent。在 Web GUI 中创建 Agent 后，你可以全程使用日常自然语言对话引导工作流：让它挂载目标代码仓库、比对近期合并的 Pull Request 与 CLI 命令输出中的文档漂移、在独立 Git worktree 中完成本地链接与构建校验，并提交带有明确代码出处引用的纯文档 PR，全程不触碰任何业务功能代码。

- 如果你需要搭建的是审阅代码 PR 的角色，请参阅[搭建持续跟进 PR 的本地 Code Reviewer](./build-code-reviewer.md)。
- 了解模板详细配置与内置技能，请参阅 [Docs Steward 模板](../templates/docs-steward/)。
- 了解多 Agent 技能与职责边界隔离设计，请参阅[按职责划分 Agent 角色与技能](./organize-agent-skills-and-roles.md)。

## 开始之前

- 本地已启动 Holon 并可访问 [Web GUI](./use-web-gui.md)（默认 `http://localhost:8080`），且已[配置可用模型](./configure-models.md)。
- 运行 Holon 的机器已安装并登录 GitHub CLI（`gh`），且已安装目标仓库所需的文档构建或链接检查工具。
- 无需手动背记复杂的 CLI 命令：关联仓库工作区、在 `AGENTS.md` 确立角色规则、运行漂移审计与创建隔离 worktree，均可直接在对话中交给 Agent 完成。

## 操作步骤

### 1. 从 `docs-steward` 模板创建 Agent

在 Web GUI 中点击 **New Agent**，选择 **Docs Steward**（`docs-steward`）模板，并为 Agent 命名（例如 `docs-steward` 或 `doc-bot`）。

`docs-steward` 模板预置了文档管家的角色契约（`AGENTS.md`）及所需技能：
- `ghx`：GitHub CLI 工具集成，用于查询已合入的 PR、读取 Issue 及提交纯文档 PR；
- `sview`：结构化查看代码、符号表与文档目录，无需全量读取大文件或执行大面积终端扫描；
- `agentinbox`：持久化事件订阅，用于监听仓库代码变更与 PR 事件；
- `writing-clearly-and-concisely`、`humanizer` 与 `humanizer-zh`：技术写作风格指南与中英文档双语镜像同步；
- `mermaid-diagrams`：经过校验的架构图与工作流图生成技能。

### 2. 对话式挂载仓库并设定权限边界

在 Web GUI 中与刚创建的 Agent 开启对话，让它挂载本地代码仓库并确认操作权限边界：

> "请关联本地仓库 `/path/to/my-repo`，并确认你的执行边界：你可以只读检查源码与 CLI `--help` 输出以核实事实，但只能修改文档类文件（`docs/`、`README.md`）。严禁修改任何业务代码、测试用例或构建脚本，严禁自主合并 PR。"

Agent 会调用 `AttachWorkspace` 挂载仓库，并将此边界规则持久化写入其 `AGENTS.md` 中。这确保了：
- **证据优先核实**：所有修改必须指向具体的源码路径、CLI 终端参数或已批准的 RFC，不虚构未实现特性。
- **纯文档变更边界**：严格限制在文档范围内，当发现代码与旧文档不一致时，坚决不反向修改代码逻辑。
- **人工审批红线**：Agent 只起草并提交 PR 供人类维护者 Review，绝不自行合并代码或发布版本。

### 3. 执行首次文档漂移巡检（First Success）

在对话中指定近期的提交或 CLI 命令，让 Agent 检查文档是否已经静默过期：

> "请检查 `main` 分支最近合入的 5 个 PR，以及 `./my-app --help` 的最新输出，找出有哪些新参数、默认值变动或子命令未在 `docs/` 或 `README.md` 中说明。列出一份包含文件路径与行号的审计清单。"

Agent 将利用 `ghx` 与 `sview` 深度核对代码改动与现存文档，并输出结构化漂移报告：

```markdown
### 文档漂移审计清单

1. **新增 `--output-format` 选项**：
   - 源码出处：`src/cli/args.rs:48`（合入自 PR #142）。
   - 缺失页面：`docs/reference/cli.md`（当前仍停留在 v1.2 版本）。
2. **默认超时时间调整**：
   - 源码出处：`src/config/defaults.rs:16`（默认值由 30s 调整为 60s，合入自 PR #145）。
   - 存在矛盾：`README.md:82` 与 `docs/guides/getting-started.md:34` 仍记录为 30s。
```

### 4. 在独立 Git Worktree 中撰写文档并提交 PR

核对上述清单后，直接在对话中指示 Agent 在独立的 Git worktree 中完成文档编写与本地校验：

> "请在独立 worktree（分支 `docs/sync-cli-v1-3`）中更新 `docs/reference/cli.md` 与 `README.md`，运行本地文档链接检查与构建，通过后提交一个 Draft PR 供审阅。"

Agent 执行以下动作：
1. 调用 `CreateWorktree` 创建独立分支目录，你的本地主工作区代码完全不受干扰；
2. 应用精准的文件补丁，补全参数说明并标注源码引用来源；
3. 运行本地文档校验脚本（如 `npm run check:content` 或 `mkdocs build`），确保无死链、无 Markdown 格式破损；
4. 通过 `ghx` 提交 Pull Request，在 PR 描述中清晰列出每一处修改对应的 PR 编号与代码行号。

你可以在 GitHub 或 Web GUI 中直接审查该 PR，无需离开当前工作环境。

### 5. 配置事件订阅实现长效持续跟进

为了让文档管家持续跟进后续的代码演进，在对话中配置事件订阅：

> "请使用 `agentinbox` 配置事件订阅，持续监听本仓库已合并的 Pull Request。当有影响 `src/` 或 `crates/` 的 PR 合入时，自动唤醒并执行文档漂移核查。"

Agent 将配置 GitHub 事件订阅。后续当新的代码合入主干时：
- Holon 收到 Webhook 事件并精准唤醒该 `docs-steward` Agent；
- Agent 创建跟踪 WorkItem，核验用户可见契约或 CLI 选项是否发生变化，必要时输出待办简报或起草 PR；
- 在没有新事件到达的静默期间，Agent 处于休眠挂起状态，不消耗任何模型 Token。

## 验收与自检

- **工作区边界检查**：在 Web GUI 中查看 Agent 工作区状态，确认任何业务代码文件（`.rs`、`.go`、`.ts` 等）均未产生未提交修改。
- **事实证据溯源**：检查 PR 描述中的每一个论点，确认均附带对应的 Commit SHA 或代码文件行号。
- **Worktree 干净回收**：PR 合入后，指示 Agent 安全清理已合入的临时 worktree 与分支。

## 常见问题与排障

- **`gh` 认证失败**：若 Agent 提示 `gh: not logged in`，请在运行 Holon 的宿主机终端执行 `gh auth login` 登录，随后让 Agent 重试。
- **缺少完整 Git 历史导致漂移漏检**：若本地仓库克隆深度较浅（shallow clone），请在对话中让 Agent 先执行 `git fetch --unshallow` 或拉取完整提交历史。
- **构建或链接校验报错**：若本地文档校验失败，可在对话中直接提示 Agent：*“检查构建日志中的失效链接与排版报错，在当前分支修复后再提交 PR。”*

## 下一步

- 阅读[使用工作区与 Git Worktree](./work-with-workspaces.md)，了解 Holon 如何安全隔离并行 Agent 开发。
- 阅读[连接外部事件与 Webhook](./connect-external-events.md)，深入了解 Holon 的事件驱动调度机制。
- 阅读[管理 Agent 记忆](./manage-agent-memory.md)，为你的文档管家沉淀长期的术语表与写作排版风格规范。
