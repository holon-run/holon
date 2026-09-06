---
title: 运行时规格
summary: 面向维护者和贡献者的当前实现侧运行时契约。
order: 1
---

# 运行时规格

规格（spec）页面描述**当前运行时契约**——Holon 运行时今天实际做了什么，
并基于实现和测试验证。

规格不是教程或用户指南。它们是给需要理解或修改运行时行为的贡献者、
维护者和集成方的权威契约。

## 规格在文档模型中的位置

| 层 | 内容 | 读者 |
|-------|------|----------|
| [指南](/guides/) | 任务导向的工作流 | 用户 |
| [概念](/zh-CN/concepts/) | 心智模型 | 用户、评估者 |
| [参考](/zh-CN/reference/) | CLI、配置、控制平面快照 | 用户、集成方 |
| **规格**（本节） | 当前运行时契约 | 维护者、贡献者 |
| [RFCs](https://github.com/holon-run/holon/tree/main/docs/rfcs) | 设计记录和理由 | 维护者 |

规格桥接用户文档和 RFC 设计历史之间的空隙。当 RFC 稳定为运行时行为后，
当前契约会被提取到这里。RFC 保留为设计记录；规格是活契约。

> 本节除本页外暂为英文，链接会跳转到对应英文页面。中文翻译在逐步补充中。

## 如何阅读规格

每个规格页面遵循一致的结构：

- **契约（Contract）** — 运行时实现的规范行为。
- **验证（Validation）** — 契约如何基于实现、测试和 RFC 检查。
- **RFCs** — 关联的源设计记录。
- **已知缺口（Known gaps）** — 未解决漂移的跟踪后续 issue。

## 当前规格页面

- [Agent 状态](/spec/agent-state.md)（英文）
  当前 Agent 状态、生命周期标签、运行时投影和用户可见的展示契约。

- [工作项](/spec/work-items.md)（英文）
  当前 WorkItem 生命周期、focus、readiness、规划、阻塞和完成契约。

- [调度器](/spec/scheduler.md)（英文）
  当前调度器输入、runnable/waiting 决策、WorkItem readiness 和 wake/sleep 边界。

- [唤醒与延续](/spec/wake-and-continuation.md)（英文）
  当前触发器分类、外部 ingress 能力、continuation 解析和 wake/sleep 生命周期。

- [任务](/spec/tasks.md)（英文）
  当前任务生命周期、terminal re-entry 和命令/子 Agent 监督契约。

- [工具](/spec/tools.md)（英文）
  当前面向模型的工具家族、权限边界、输入/结果契约和已废弃接口。

- [Workspace 与执行](/spec/workspace-and-execution.md)（英文）
  当前 workspace 身份、agent home、execution root、worktree 和 host-local 策略契约。

- [信任与来源](/spec/trust-and-provenance.md)（英文）
  当前 provenance、admission/authentication、指令权威和执行策略契约。

## 与 `docs/runtime-spec.md` 的关系

[`docs/runtime-spec.md`](https://github.com/holon-run/holon/blob/main/docs/runtime-spec.md)
现在是一个聚合索引，把最初的 v0 单体规格映射到当前的聚焦规格页面。
它不再包含规范内容。

**这里的聚焦规格页面是唯一的权威实现侧契约。** 当某个主题有专门的规格页面时，
该页面就是当前权威。

## 给贡献者

当你修改运行时行为时，同步更新相关规格页面和 RFC。规格页面基于实现验证——
如果实现和规格不一致，修复错误的一方，并为另一方开 issue。

<!-- INDEX:START -->

<!-- INDEX:END -->
