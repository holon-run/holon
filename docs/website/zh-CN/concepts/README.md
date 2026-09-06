---
title: 概念
summary: Holon 背后的心智模型——让 Agent 工作持久化的四个简单对象。
order: 20
---

# 概念

把 Holon 理解为一个运行时（runtime），而不是一个聊天外壳，是最容易的入门方式。
这个运行时围绕四个简单对象构建，它们相互配合：

## 四个对象的心智模型

**Agent（代理）** 是行动者。每个 Agent 有持久的 home 目录、自己的指导文件
（`AGENTS.md`）、本地记忆和工作队列。你通过 ID 寻址一个 Agent，它跨轮次存在——
可以休眠、唤醒、继续工作而不丢失状态。

**Work item（工作项）** 是目标。它记录 Agent 想要达成*什么*，带有持久计划、
todo 清单和完成目标。工作项跨轮次、跨模型调用存活，所以 Agent 几小时或几天后
恢复也不会丢失进度。

**Task（任务）** 是执行。每条命令、每次子 Agent 委托、每个后台操作都包装在
任务句柄里。你可以检查任务状态、读取输出、发送输入或停止任务——运行时全程跟踪。

**Trust boundary（信任边界）** 是来源。操作者输入、外部 webhook、子 Agent 输出、
网页来源内容，各自携带来源（origin）和信任级别。Holon 从不把它们压平成一条
无差别的提示流。

```
Agent（谁）
  └── Work items（在做什么）
        └── Tasks（怎么做）
              └── Trust classification（输入来自哪里）
```

## 为什么这很重要

大多数 Agent 工具像聊天会话：提示进去，回答出来，状态消失。Holon 不一样：

- 你可以离开，Agent 继续工作。
- 你不用读完整个对话记录就能检查 Agent *正在做*什么。
- 你能看到每个输入*来自哪里*、可信度多高。
- 你可以把工作委托给子 Agent 并监督它们的进度。

## 深入阅读

**记忆系统**页面解释 Holon 如何通过工作项、work refs、episode 归档和索引搜索，
跨轮次保持连续性。（[英文](/concepts/memory)）

**上下文连续性**页面解释 Holon 如何从轮次、工作项、任务结果、简报和结构化
episode 中组装有界的提示上下文，而不依赖原始对话回放。（[英文](/concepts/context-continuity)）

**运行时模型**页面把四对象心智模型展开为精确的生命周期词汇：Agent profile、
工作项状态、任务种类、队列语义、触发器和 workspace 隔离。（[英文](/concepts/runtime-model)）

**信任边界**页面面向产品：它解释 origin 和 trust 对真实集成为什么重要，
而不只是安全理论。（[英文](/concepts/trust-boundaries)）

**文档分层**页面解释 Holon 如何区分产品文档、当前契约参考和维护者设计记录——
让你知道哪类文档在哪种用途下是权威的。（[英文](/concepts/documentation-layers)）

这些概念背后的权威设计契约，见仓库的
[RFCs](https://github.com/holon-run/holon/tree/main/docs/rfcs) 和
[实现决策](https://github.com/holon-run/holon/tree/main/docs/implementation-decisions/)（英文）。
这些是面向维护者的文档；使用 Holon 不需要它们。

## 本节页面

- [运行时模型](/concepts/runtime-model.md)（英文）
  Agent、任务、工作项、工作区，以及构成 Holon 运行时的执行循环。

- [上下文连续性](/concepts/context-continuity.md)（英文）
  Holon 如何在不回放每一轮对话的情况下保持长生命周期的 Agent 上下文连贯。

- [记忆系统](/concepts/memory.md)（英文）
  Holon 的记忆分层如何跨轮次保持连续性——工作项、work refs、episode、持久账本和索引搜索。

- [文档分层](/concepts/documentation-layers.md)（英文）
  Holon 如何区分产品文档、当前契约参考和维护者设计记录。

- [信任边界](/concepts/trust-boundaries.md)（英文）
  Holon 如何分类 origin、trust 和 priority，保障长生命周期 Agent 的安全。

- [外部触发器](/concepts/external-triggers.md)（英文）
  Holon Agent 如何通过 webhook 唤醒端点和回调 URL 等待并接收外部事件。

- [安全与执行边界](/concepts/security-and-execution-boundaries.md)（英文）
  Holon 沙箱化什么、不沙箱化什么：本地执行、workspace 绑定、远程访问、能力密钥和信任元数据。

<!-- INDEX:START -->

<!-- INDEX:END -->
