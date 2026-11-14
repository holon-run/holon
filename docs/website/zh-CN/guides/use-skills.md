---
title: 为 Agent 发现、启用与验证扩展技能
summary: 在自然语言对话中为 Agent 扩展专项能力，在 AGENTS.md 中固化权限边界，并通过轻量测试用例即时验证，无需死记硬背底层 CLI 命令。
order: 40
---

# 为 Agent 发现、启用与验证扩展技能

随着你的 AI 编程助手承担更广泛的工程职责（例如审查 GitHub PR、解析与生成 Office 文档如 `docx`/`xlsx`、或执行安全合规审计），你需要一种轻量的方式为它注入专业领域的经验与工作流，同时避免将所有工具规则一股脑塞入默认提示词而导致上下文爆炸或越权风险。

在 Holon 中，专项能力被模块化封装为 **技能（Skills）**。一个 Skill 就是一份存放在特定目录下的 `SKILL.md` 工作流指南与工具配方。Holon 采用“两层管理”架构：Skill 在宿主机全局的 **Skill Library** 中统一注册和缓存，各个 Agent 则按需独立启用。

最关键的是，你完全不需要在终端里敲复杂的 CLI 子命令。你可以在 Web GUI 或移动端对话中，直接用自然语言要求 Agent 检查自身能力、启用新技能、将授权边界固化到 `AGENTS.md`，并在安全的最小任务中即时验证。

本指南介绍如何在对话中发现技能、按需启用并配置权限、通过最小测试任务验证其安全性，以及在任务结束后平滑停用。关于技能底层的发现协议与工具调度，请参阅 [Skill 与工具概念](../concepts/skills-and-tools.md)与 [Skill 语法参考](../reference/skills.md)。

## 开始之前

- 正在运行的 Holon 实例与 [Web GUI](./use-web-gui.md)（默认地址 `http://localhost:8080`）。
- 处于活跃状态的目标 Agent（例如 `developer` 或 `main`）。
- 明确的专项任务诉求（例如 GitHub PR 评审、专业文档提取或代码健康度巡检）。
- **理解安全与权限边界：** 启用 Skill 只会为 Agent 注入对应的专家工作流指南与工具调用方法，绝不会自动放开系统底层权限。涉及高风险操作（如推送远程分支、合并 PR、修改线上配置）必须由用户在 `AGENTS.md` 中显式授权。

## 操作步骤

### 1. 在自然语言对话中自检能力并发现可用技能

无须去翻阅冗长的技能目录列表，在 Web GUI 聊天窗口中直接向 Agent 发问：

> “你能帮我审查 GitHub PR #42 并核对是否有破坏性 API 变更吗？检查你是否具备相关的代码审查技能。”

**背后的运行逻辑：**
- Agent 自动调用自身环境检查契约，核对当前已启用的技能列表。
- 如果尚未启用（例如缺少 `ghx` 或 `code-review` 技能），Agent 会检索全局 Skill Library 或官方模板清单中的推荐项。
- Agent 会如实向你汇报能力边界与缺失项：
  > “*我目前具备本地代码分析与 Git 检查能力，但尚未为我启用 `ghx`（GitHub CLI）与 `code-review` 技能，无法直接读取 PR #42 的元数据与 Diff。是否需要我为你启用官方库中的 `ghx` 技能？*”

### 2. 指示 Agent 启用技能并固化安全边界

在对话中直接授权 Agent 启用该技能，并给出明确的权限限制：

> “好的，请为当前 Agent 启用 `ghx` 技能。注意：你只拥有读取 PR Diff 和撰写评审草稿的权限；未经我显式确认，严禁提交正式 Review 或执行合并。”

**背后的运行逻辑：**
1. **技能库激活**：Agent 调用 Holon 的内置技能管理接口，从全局共享库中为当前 Agent 绑定并启用 `ghx`。
2. **权限固化**：Agent 自动将上述限制写入其受管的 `AGENTS.md` 规则中（如只读审查、禁止自动 merge），确保后续各轮会话均严格遵循此约束。
3. **按需上下文隔离**：Holon 仅将技能元数据注册到 Agent 的生效目录中，而不会一开始就将整个 `SKILL.md` 塞入上下文，只有在真正触发该工作流时才会按需阅读。

Agent 会在对话中明确回复：
> “*已成功为当前 Agent 启用 `ghx` 技能。我已将‘仅只读审查 PR、严禁未经确认执行合并’写入操作规则，随时可以开始工作。*”

### 3. 用轻量测试任务即时验证

在正式交付核心代码前，先通过一个安全的只读小任务验证技能是否正常运转：

> “抓取 PR #42 的变更元数据与 Diff，在对话中输出一段 3 点要点摘要。不要在 GitHub 上发表任何评论。”

**在 Web GUI 中观察执行过程：**
1. **技能按需阅读**：Agent 识别到当前任务匹配刚刚启用的 `ghx` 技能，首先读取本地 `SKILL.md` 文件，确认命令调用格式与前置约束。
2. **安全工具调用**：Agent 执行只读命令（如 `gh pr view 42`）抓取数据。
3. **严格遵守边界**：在对话中精准呈现 3 点要点总结，且没有触发任何发帖或写入操作。

### 4. 任务结束后的平滑停用与清理

当临时专项任务结束后（例如完成了一次性的跨平台文档迁移或合规审计），可以在对话中指示 Agent 停用该技能以保持专注：

> “PR 审查工作已结束，请为当前 Agent 停用 `ghx` 技能，保持最小权限原则。”

Agent 会将其从自身的生效列表中移除。该技能依然完好保留在宿主机全局 Skill Library 中，其他 Agent 或未来需要时可随时再次启用。

---

## 可选：面向自动化运维的 CLI 与模板声明

对于需要编写自动化容器镜像、CI/CD 跑道或无头初始化的开发者，Holon 提供了完整的底层 CLI 命令。

### 通过 CLI 管理 Skill Library

```bash
# 查看共享库中已安装的所有技能
holon skills catalog

# 从本地目录或远程 GitHub 仓库添加技能到全局共享库
holon skills add /path/to/local/skill
holon skills add holon-run/holon/skills/ghx@main

# 为指定 Agent 启用特定技能
holon skills enable ghx --agent developer

# 查看指定 Agent 当前生效的技能列表
holon skills list --agent developer

# 检查技能版本更新与 Lock 文件差异
holon skills check ghx
holon skills update ghx

# 为特定 Agent 停用技能
holon skills disable ghx --agent developer

# 从全局共享库中彻底移除技能
holon skills remove ghx
```

### 在 Agent 模板清单中预声明技能

如果你正在沉淀标准化的团队 Agent 模板（参见[Agent 模板参考](../reference/agent-templates.md)），可以在模板目录的 `skills.toml` 中声明预装技能：

```toml
[skills]
required = [
  "holon-run/holon/skills/ghx",
  "holon-run/holon/skills/code-review"
]
```

基于该模板创建 Agent 时，Holon 会自动为其挂载这些预设技能。

## 后续探索

- [Skill 与工具概念](../concepts/skills-and-tools.md)：深入理解 Holon 的工具调度机制、安全隔离与模型上下文设计。
- [在项目工作区与 Git Worktree 中工作](./work-with-workspaces.md)：为 Agent 挂载独立代码仓库与隔离的分支工作区。
- [实时观察、追踪与验收 Agent 工作进展](./observe-agent-work.md)：实时跟踪 Agent 执行动态与产物交付。
