---
title: 观察 Agent 工作
summary: 定位 Agent 的工作目标、查看任务进展并检索最终完成报告与交付产物。
order: 250
---

# 观察 Agent 工作

排查工作时应首先检查 Agent 与工作项，只有在确实需要具体执行细节时才去调阅任务日志。检查单项工作的进展无需专门搭建全套指标采集系统。

## 前置条件

明确目标 Agent ID，并在宿主机上运行操作者命令（例如通过已认证的终端会话）。远程 TUI 连接不会自动重定向后续的 Shell 命令。以下示例使用 `builder`，请替换为你的实际 Agent ID。务必显式传入 `--agent`，以防误读默认 Agent。

## 1. 定位工作目标与状态

```bash
holon agent status builder
holon work-item list --agent builder
holon work-item get WORK_ITEM_ID --agent builder
```

从列表中复制目标 `WORK_ITEM_ID`。检查其目标陈述（Objective）、计划状态（Plan status）、Todo 清单、等待原因（Wait reason）以及完成结果字段。`current` 仅表示当前焦点，不代表当前一定有代码在运行。处于 `yielded` 状态的工作项表示它正在等待延续恢复。

## 2. 检查后台任务执行细节

如果工作项正在执行耗时较长的后台命令或委托子任务：

```bash
holon task list --agent builder
holon task status TASK_ID
holon task output TASK_ID --limit 50
```

`TaskStatus` 展示任务当前的生命周期（运行中、已取消、已完成等）以及是否接受交互输入；`TaskOutput` 打印受限的最新输出。

## 3. 获取最终交付成果与完成报告

当工作项完成后，查看其权威完成报告：

```bash
holon work-item get WORK_ITEM_ID --agent builder
```

重点阅读 `result_summary` 与关联的交付文件路径。交付物通常存放在明确的工作目录（如 `work/`、`notes/` 或具体的 Git worktree 路径）中。确认产物路径并检查验证结论，不要把中间调试日志误当成最终交付。

## 另见

- [交付结果、产物与可观察状态概念](/zh-CN/concepts/results-and-artifacts.md)
- [工作项参考](/zh-CN/reference/work-items.md)
- [配置可观测性](configure-observability.md) — 导出生产环境 Trace 与指标
- [排查 Holon 任务问题](troubleshooting.md)
