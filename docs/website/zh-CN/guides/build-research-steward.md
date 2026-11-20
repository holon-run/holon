---
title: 用 research-steward 模板构建可溯源的调研与决策简报工作流
summary: 在 Web GUI 中从 research-steward 模板创建专用调研 Agent，通过自然语言驱动问题界定、一手来源台账、引用核验与结构化决策简报生成，并跨会话跟进待核验队列。
order: 86
---

# 用 research-steward 模板构建可溯源的调研与决策简报工作流

本指南介绍如何在 Web GUI 中使用 `research-steward` 模板搭建长期调研 Agent，把开放式评估问题收敛为有边界的**问题卡（question-card）**、可追溯的**来源台账（`SRC-*`）**、经过核验的**主张/证据矩阵（`CLM-*`）**以及附带置信度和**后续核验队列（`FU-*`）**的决策简报。

- 如果需要在此模板之上持续跟踪多个开源代码库、对比 Commit 与发布动态，请参阅[搭建 AI Agent 生态研究员：跨多开源项目的持续深度调研](./build-agent-ecosystem-researcher.md)。
- 查看模板定义与预置技能详情，请参阅 [Research Steward 模板](../templates/research-steward/)。
- 该工作流用于沉淀结构化证据并保留反证供人类决策，不能替代专业领域审查，也不代表授权对外发布结论。

## 前置条件

- Holon 运行时已启动，[Web GUI](./use-web-gui.md) 可访问（默认 `http://127.0.0.1:7878/`），且已配置可用模型路由（见[配置模型](./configure-models.md)）。
- 如需检索公开文档或行业公告，已配置联网搜索与抓取（见[使用 Web 搜索与网页抓取](./use-web-tools.md)）；如需对照源码，本机已具备 `gh` 或可挂载的目标仓库。
- 无需手动初始化目录或编写 YAML：模板会自动写入角色契约（`AGENTS.md`）并装配全套调研 Skill。

## 操作步骤

### 1. 在 Web GUI 中从 `research-steward` 模板创建 Agent

打开 Web GUI 的 **Agent Templates** 页面（`/templates`），选择 **`research-steward`**，点击 **Create Agent** 并填写一个空闲的语义化 ID（如 `research-steward` 或 `tech-research`）；也可以直接在对话中让 `main` 代为创建。

该模板会在新 Agent 独立的 `agent_home` 中默认绑定 6 项 Skill：

- `research-artifact`：按 `question-card`、`search-plan`、`source-ledger`、`claim-matrix`、`decision-brief`、`follow-up-queue` 与 `provenance` 七段式结构输出研究包。
- `citation-verifier`：逐条核查主张与来源之间的蕴含关系、适用范围、时效性与反证，未通过核验不标记为 `supported`。
- `ghx` 与 `sview`：把 GitHub Issue、Release 与代码库文件作为一手来源精准读取，避免整库倾倒污染上下文。
- `agentinbox`：管理跨会话的待核验队列与异步跟进事项。
- `writing-clearly-and-concisely`：仅在证据核验完成后润色表达，避免流畅文风掩盖薄弱证据。

> **可选（CLI）：** 无头服务器上可运行 `holon agent create tech-research --template research-steward`。模板安装细节见[在 Web GUI 中从模板创建首个专用 Agent](./create-agent.md)。

### 2. 在对话中界定问题卡与操作边界

在 Web GUI 中打开新建调研 Agent 的对话窗口（或在手机客户端随手转发资料链接），用一段话明确决策目标、受众、时间窗口与读写边界：

> “请建立一份研究任务（`Q-001`），对比团队在自部署工作流编排场景下应选择 Tool A 还是 Tool B。
> - 受众：下周评审 Q1 架构的技术负责人。
> - 时间窗口：优先核对近 6 个月内的正式发布说明与官方文档，并记录访问日期。
> - 验收标准：把官方文档、源码仓库与 Release 记录等一手来源和博客观点分开，保留冲突证据，未核实项必须显式标明。
> - 边界：仅限只读调研与分析，不得改动项目文件、发邮件或对外发布。”

如果关键定义或适用范围仍有缺口，模板契约会要求 Agent 主动发问并挂起等待（`WaitFor` `operator_input`），而不是自行脑补假设。由于工作进度持久化在守护进程的 `WorkItem` 中，你也可以在手机端丢入链接发起后台调研，回到桌面后直接审阅结构化简报。

### 3. 要求先建一手来源台账并执行引用核验

在让 Agent 输出最终结论之前，明确要求其先完成来源台账与逐条核验：

> “在撰写决策简报前，先建立 `source-ledger`（`SRC-001` 起），并对每条核心主张（`CLM-001` 起）运行 `citation-verifier`。搜索结果摘要仅作线索，需补充反证以及版本或场景限制。”

在 `research-steward` 契约约束下：

- 每条重要来源都会分配稳定编号（`SRC-001`），记录标题/发布方、精确定位器、发布或更新时间、访问日期、适用范围及**不能证明什么**。
- 每条核心主张都会获得稳定编号（`CLM-001`）和唯一终态：`supported`、`partially-supported`、`contradicted`、`unverified` 或 `blocked`。
- 相互冲突的证据会并列保留，并说明分歧是源于统计口径、时间窗口、版本差异还是来源质量，不会为了结论好听而悄悄忽略反例。

### 4. 审阅决策简报与后续核验队列

核验完成后，按固定七段结构检查产出的 `research-artifact` 研究包：

```markdown
1. question-card     — 决策问题、受众、范围、时间窗与验收标准
2. search-plan       — 子问题拆解、来源类型、时间边界与刻意排除项
3. source-ledger     — SRC-001..N 精确定位器、日期与来源局限性
4. claim-matrix      — CLM-001..N 与 SRC-* 及 citation-verifier 状态映射
5. decision-brief    — 结论先行、证据支撑、竞争解释、限制与置信度
6. follow-up-queue   — FU-001..N 未决事项、阻塞原因与关闭条件
7. provenance        — 生成者、核验时间与访问受限说明
```

几周后若需复查同一主题，直接让同一个 Agent 更新指定 `SRC-*` 或 `FU-*` 条目即可。它会保持原有编号稳定并追加修订记录，而不会静默改写历史证据。你也可以在对话中把偏好的简报篇幅或回复语言写入 `memory/operator.md`，详见[管理 Agent 记忆](./manage-agent-memory.md)。

## 核验要点

在采纳简报做决策前，从对话结果或导出的 Markdown 产物中核对三项事实：

1. **定位器可回溯**：`claim-matrix` 中每条 `supported` 主张都关联了 `SRC-*` 与具体章节、Commit、Release 或 URL，而不是搜索引擎摘要或首页链接。
2. **不确定性显式可见**：无法访问的页面、过时数据或冲突来源留在 `unverified` / `blocked` 行与 `follow-up-queue`（`FU-*`）中，没有被静默抹平。
3. **边界未被越权**：展开执行详情确认该轮仅进行了只读检索与分析，没有未授权的代码修改、提交或外部发布。

## 故障排查

- **简报像泛泛的搜索拼凑**：要求 Agent 严格先填充 `source-ledger` 并运行 `citation-verifier`，再生成 `decision-brief`，或直接把一手文档 URL/本地代码库喂给它。
- **外部网页返回 403 或登录拦截**：保留受影响主张为 `unverified` 或 `blocked`，必要时由人工补充授权片段或本地文件，切勿让 Agent 猜测缺失数据。
- **多轮调研范围越滚越大**：在对话中收窄 `question-card` 的验收标准；如果是长期跨多仓库跟踪演进，参阅[搭建 AI Agent 生态研究员](./build-agent-ecosystem-researcher.md)拆出独立工作流。

## 另见

- [搭建 AI Agent 生态研究员：跨多开源项目的持续深度调研](./build-agent-ecosystem-researcher.md)
- [使用 Web 搜索与网页抓取](./use-web-tools.md)
- [管理 Agent 记忆](./manage-agent-memory.md)
- [创建并定制专属角色 Agent 与权限边界](./customize-agent.md)
- [信任边界](../concepts/trust-boundaries.md)
