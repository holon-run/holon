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

这些概念背后的权威设计契约，见仓库的
[RFCs](https://github.com/holon-run/holon/tree/main/docs/rfcs) 和
[实现决策](https://github.com/holon-run/holon/tree/main/docs/implementation-decisions/)（英文）。
这些是面向维护者的文档；使用 Holon 不需要它们。

<!-- INDEX:START -->

- [运行时模型](./runtime-model.md)
  Agent、任务、工作项、工作区，以及构成 Holon 运行时的执行循环。
  <!-- mdorigin:index kind=article -->

- [上下文连续性](./context-continuity.md)
  Holon 如何在不重放全部对话记录的前提下，让长期 Agent 的上下文保持连贯。
  <!-- mdorigin:index kind=article -->

- [记忆系统](./memory.md)
  Holon 的记忆分层如何跨轮次保持连续性——工作项、work refs、片段、持久账本和索引搜索。
  <!-- mdorigin:index kind=article -->

- [多 Agent 协作](./multi-agent-collaboration.md)
  一个 Agent 把工作委派给另一个 Agent 时的角色、边界和信任。
  <!-- mdorigin:index kind=article -->

- [信任边界](./trust-boundaries.md)
  Holon 如何对来源、信任和优先级分类，让长期 Agent 保持安全。
  <!-- mdorigin:index kind=article -->

- [文档分层](./documentation-layers.md)
  Holon 如何区分产品文档、当前契约参考和维护者设计记录。
  <!-- mdorigin:index kind=article -->

- [外部触发器](./external-triggers.md)
  Holon Agent 如何通过 webhook 唤醒端点和回调 URL 等待并接收外部事件。
  <!-- mdorigin:index kind=article -->

- [安全与执行边界](./security-and-execution-boundaries.md)
  Holon 沙箱化与不沙箱化的部分：本地执行、工作区绑定、远程访问、能力密钥和信任元数据。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
