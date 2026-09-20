---
title: 记忆系统
summary: Holon 的记忆分层如何跨轮次保持连续性——工作项、work refs、片段、持久账本和索引搜索。
order: 15
---

# 记忆系统

Holon 是为长期 Agent 设计的。记忆让重要的东西跨轮次保留下来，而不必重放整份
对话历史。运行时从持久证据推导记忆，而不是依赖自由格式的模型总结。

## 记忆索引

Holon 在启动时异步建立记忆索引。索引构建在后台运行，**不会阻塞守护进程
启动**。新事件写入持久账本后会继续被索引。

初次建立索引期间，搜索结果可能不完整。运行时优先处理新写入的事件，因此即使
后台还在追历史记录，近期记忆也总能被搜到。

## 记忆分层

Holon 的上下文记忆分四层，各司其职：

```
Durable Ledger  ──── append-only audit trail (messages, briefs, tool calls, tasks)
     │
     ▼
Current Work Context ─ current WorkItem, todo state, waits, and refs
     │
     ▼
Episode Memory  ──── archived records of completed work chunks
     │
     ▼
Context Assembly ──── budgeted prompt sections selected from all layers
```

### 持久账本

只追加的事实来源。每个运行时事件都会被记录：

- 消息和简报
- 工具执行和命令结果
- 任务生命周期转换
- 工作项状态变化和当前 work refs
- 片段记录

账本**不受提示长度约束**，作为审计轨迹无限增长。模型可见的投影是压缩后的
选取，不是完整重放。

### 当前工作上下文

当前工作上下文是运行时自己的紧凑投影，回答：

- 现在有什么工作在进行？
- 当前目标和计划是什么？
- 哪些 todo、等待和阻塞是重要的？
- 哪些文件、工具输出、issue、PR、任务或记忆应该保持容易重新打开？

面向提示词的权威是当前 `WorkItemRecord` 及其运行时推导的 `work_refs`。
work refs 在轮次收尾时从可信运行时证据中提取，例如当前输入的 source refs 和
工具执行记录。模型不直接编写它们。

当前工作上下文**不是自由格式总结**，而是运行时自有记录的结构化投影：当前
WorkItem 状态、活动 todo 列表、阻塞、等待条件和指回可检索证据的引用。

### 片段记忆

片段记忆归档已完成的工作。工作进行中时，一个活动片段构建器会累积：

- 活动工作项 ID 和投递目标
- 工作摘要和范围
- 触及的文件
- 验证证据
- 做出的决策
- 需要带到后续的跟进事项

到达有意义的边界（工作项完成、任务结束）时，运行时把构建器定稿为不可变的
**片段记录**并保存。

归档片段按相关度和预算被选入提示上下文，默认不会完整渲染。默认提示拼装把
片段当作中期存档：它会排除与 `recent_turns` 窗口重叠的片段，避免同一批轮次
既以原始证据又以摘要重复出现。

### 上下文拼装

每个轮次都从有预算的记忆分段拼装提示词：

- 近期轮次上下文（当前输入、延续、近期事件）
- 基于轮次的上下文投影（关联轮次、结果简报、任务结果和工作项转换）
- 当前工作项和计划
- 当前 work refs
- 相关片段记忆
- 执行环境投影

这种拼装让提示规模保持有界，同时保住连续性。变化较慢的记忆分段能保持 provider
缓存身份稳定。关于这些分段如何协作的用户侧说明，见
[上下文连续性](/zh-CN/concepts/context-continuity.md)。

## 记忆与 Agent Home 文件

记忆和 agent home 文件用途不同：

| 方面 | 运行时记忆 | Agent Home 文件 |
|--------|---------------|-----------------|
| **存什么** | 当前状态、片段、证据 | 角色契约、笔记、参考资料 |
| **谁写** | 运行时（自动） | Agent 或操作者（手动） |
| **持久性** | 只追加账本 + 快照 | 持久文件 |
| **搜索** | 通过 `MemorySearch` 建立索引 | 普通文件读取 |
| **加载** | 有预算的提示拼装 | `AGENTS.md` 始终加载 |

`agent_home/AGENTS.md` 是始终加载的指引，即 Agent 的长期角色契约。运行时记忆
是自动推导的证据。两者并存，互不重叠。

## MemorySearch 与 MemoryGet

Holon 提供两个用于索引检索的记忆工具：

- **`MemorySearch`** — 按查询在记忆来源（agent 记忆 markdown、运行时证据）中
  搜索，返回带不透明 `source_ref` 的排序结果。
- **`MemoryGet`** — 按 `source_ref` 获取确切的记忆内容，用于取回搜索定位到的
  具体记录。

这两个工具让 Agent 按需拉取相关的过往上下文，而不必把每个归档片段都塞进每
一次提示。

## Agent 记忆自动加载

Holon 会自动把 Agent 记忆文件的一小段注入每个轮次的系统提示词。这样 Agent
无需手动回忆或搜索，就拥有持久的自我认知和操作者偏好。

参与自动加载的有两个文件：

| 文件 | 用途 | 谁写 |
|------|---------|---------------|
| `agent_home/memory/operator.md` | 操作者偏好、长期指令 | 操作者 |
| `agent_home/memory/self.md` | Agent 自我认知、角色事实 | Agent |

轮次拼装时，每个文件都会被读取，并在固定的单文件字符预算下注入一个**紧凑
切片**（默认 1500 字符）。如果文件超过预算，注入的切片会被截断，Agent 会
收到一条说明，指出其余内容可通过 `MemoryGet` 取回。如果文件为空，Agent 会
收到一条说明，指出尚未有整理好的内容。

自动加载的分段在提示词中显示为：

- **`agent_memory_operator`** — 整理过的操作者记忆，以 `Stability::AgentScoped`
  加载（只在操作者编辑该文件时变化）。
- **`agent_memory_self`** — 整理过的自我记忆，以 `Stability::AgentScoped`
  加载（只在 Agent 编辑该文件时变化）。

这两个分段在提示层级中位于 `AGENTS.md` 指引和工作区作用域之间。它们的权威
低于工作区级或轮次级指令，但提供能跨上下文压缩存活的持久事实。

### 各文件的使用时机

- **`operator.md`** — 存放跨 Agent 的操作者偏好：偏好的语言、命名约定、工具
  默认值、沟通风格。这些与正在运行哪个 Agent 无关。
- **`self.md`** — 存放 Agent 自己的持久事实：角色、长期职责、值得记住的过往
  决策、反复出现的工作流笔记。

## 笔记目录

除了整理好的记忆文件，Holon 还可以把 Agent `notes/` 目录的元数据目录注入
提示词。笔记目录是有界的参考索引，不是指令内容。

目录由 `agent_home/notes/` 下的每个 Markdown 文件渲染而成，包含：

- **标题** — 从 frontmatter、第一个标题或文件名提取。
- **摘要** — 从 frontmatter 或第一段摘录提取。
- **标签** — 从 frontmatter 提取（转小写、去重）。

目录有界：最多 20 条、总计 2000 字符。笔记正文**绝不会**被注入——目录只是
元数据索引。Agent 可以通过读取引用的文件获取完整笔记内容。

笔记被视为参考资料，不是指令。它们不会覆盖操作者输入、AGENTS.md 指引或
当前 WorkItem 目标。

## 记忆与工作项

记忆与工作项紧密耦合：

- **当前工作项** 锚定实时连续性状态：目标、计划、todo 列表、阻塞状态和当前
  work refs。
- **片段记录** 以工作项为范围。工作项完成时，它累积的证据变成归档片段。
- 如果当前工作项和某个片段同时命中提示词，工作项在“当前目标、计划、todo 和
  等待状态”上仍然是权威；片段是支撑性历史证据。
- **MemorySearch** 在已完成的片段上建立索引，让过往工作可以按内容而不是只按
  时间戳找到。

这意味着 Holon 能记住它为之前某个 issue 做了什么，而不必重读那段工作的整份
对话记录。

## 记忆边界

Holon 按身份作用域区分记忆：

- **Agent 级记忆** — 工作记忆、片段和搜索索引属于特定 Agent。
- **工作区级记忆** — 片段记录会打上工作发生地 `workspace_id` 的标签。
- **整理过的持久记忆** — `agent_home/memory/self.md` 和
  `agent_home/memory/operator.md` 是手动整理的 Markdown 文件，用于 Agent
  特有事实和操作者偏好。

这些边界避免会话对话记录成为唯一的记忆面，也让共享工作区能在多个 Agent 之间
累积知识。

## 另见

- [上下文连续性](/zh-CN/concepts/context-continuity.md) — Holon 如何在不重放整份对话记录的前提下保持上下文连贯
- [运行时模型](/zh-CN/concepts/runtime-model.md) — Agent、工作项和执行循环
- [文档分层](/zh-CN/concepts/documentation-layers.md) — 记忆在 Holon 文档架构中的位置
- [Agent 模板](/zh-CN/reference/agent-templates.md) — 模板如何初始化 Agent 角色契约
- RFC：[长期上下文记忆](https://github.com/holon-run/holon/blob/main/docs/rfcs/long-lived-context-memory.md)
- RFC：[Agent 与工作区记忆](https://github.com/holon-run/holon/blob/main/docs/rfcs/agent-and-workspace-memory.md)
