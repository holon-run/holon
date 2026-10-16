---
title: 观察 Agent 工作
summary: 定位 Agent 的工作目标、查看任务进展并获取最终完成报告与交付产物。
order: 250
---

# 观察 Agent 工作

排查工作时应先查看 Agent 与工作项，只有需要具体执行细节时才去调阅任务状态。检查单项工作的进展无需搭建专门的指标采集系统。

## 前置条件

明确目标 Agent ID，并在宿主机上运行操作者命令（例如通过已认证的终端会话）。远程 TUI 连接不会自动重定向后续的 Shell 命令。以下示例使用 `builder`，请替换为你的实际 Agent ID。务必显式传入 `--agent`，以防误读默认 Agent。

## 1. 定位工作目标

```bash
holon agent status builder
holon work-item list --agent builder
holon work-item get WORK_ITEM_ID --agent builder
```

从列表中复制目标 `WORK_ITEM_ID`。检查其目标陈述（Objective）、计划状态（Plan status）、Todo 清单、等待原因（Wait reason）以及完成结果字段。`current` 仅表示当前焦点，不代表当前一定有代码在运行。处于 `yielded` 状态的工作项表示它正在等待延续恢复。

如果列表中没有匹配项，说明该工作可能是一次未持久化跟踪的轻量请求。此时应检查最近输出以及请求的 message ID 或 task ID，无需仅仅为了观察而新建工作项。

## 2. 查看最新简报与实时进展

```bash
holon tail --agent builder --limit 10
holon tail --agent builder --include-progress --follow \
  --timeout-ms 5000 --max-events 100 --max-bytes 65536
```

第一条命令读取最近的交付简报（Brief）。第二条命令开启持久化 Assistant 进度流，返回以 `end` 回执结尾的有界 NDJSON 数据。如需继续跟进，保存返回的游标（Cursor）即可：

```bash
holon tail --agent builder --include-progress --follow --cursor 'RETURNED_CURSOR'
```

Follow 超时仅表示本次观察窗口结束，不代表任务失败。若遇到游标重置或断档，不带游标重新抓取最新快照即可。若运行时未声明 `agents.conversation-read.v1`，使用默认的简报快照即可。

在后台命令运行期间，中间进度可能暂时空白。这类读取不会唤醒 Agent、不会消费任务结果，也不会按任务隔离输出；同一个 Agent 的输出可能包含多个任务的记录。

## 3. 检查具体后台任务

使用执行回执或任务列表中获取的 task ID：

```bash
holon task list --agent builder
holon task status TASK_ID --agent builder
holon task output TASK_ID --agent builder
```

核对任务归属者与工作项 ID 是否与目标一致。即便 Agent 切换了焦点，任务依然保留最初的拥有者。检查退出状态与输出日志中的报错信息；已结束（terminal）的任务并不一定代表成功完成。

若 Agent 正在等待任务完成，应使用 `WaitFor(wake=task_result)` 关联该 ID。反复读取任务状态只用于人工诊断，不能替代 Agent 运行时的持久等待机制。

## 4. 获取最终完成结果与产物

工作项标记完成后，再次读取其详情。确认 `state=completed`，并通过 `result_brief_id` 定位权威完成报告。最近的简报可能混杂了确认回复与等待日志，不要假定最新文本一定属于当前目标。

当工作项完成后，查看其权威完成报告：

```bash
holon work-item get WORK_ITEM_ID --agent builder
```

完成报告中应说明代码变更、验证结果、已知限制以及产物所在位置。前往返回的 workspace 或 worktree 路径下检查对应产物。在其他检出目录下同名的文件不一定是最终交付物。关于通过 HTTP 接口精确检索报告的方法，参见[通过 HTTP 自动化操作](automate-over-http.md#check-agent-status)。

## 5. 必要时升级排查与人工介入

如果状态和简报仍无法解释问题，可以调阅有界的完整对话记录：

```bash
holon transcript --agent builder --limit 30
```

记录 Agent ID、工作项 ID、任务 ID、消息 ID 以及观察到的具体异常。在共享记录或日志前，请先脱敏密钥凭证与无关内容。

排查成功的标志是：能够确认目标的当前状态或完成报告、找到其交付产物，并将未完成的工作与观察超时区分开来。若执行停滞，参见[排查故障](troubleshooting.md)。若需配置全运行时遥测，参见[配置可观测性](configure-observability.md)。
