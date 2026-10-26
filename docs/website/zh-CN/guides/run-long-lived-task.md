---
title: 运行长生命周期任务
summary: 在关闭终端后让 Agent 继续工作，等待外部依赖并在新会话中找回同一任务结果。
order: 70
---

# 运行长生命周期任务

在关闭终端或断开客户端后，后台运行的守护进程仍可继续推进工作。当任务需要跨会话持续推进，或需要等待外部决策、CI 运行及其他后台任务时，请使用 WorkItem（工作项）。本指南演示如何将单一目标从规划推进至最终验收。持久化记录让 Agent 能够在跨会话重连后恢复其目标；但这并不保证底层 Shell 进程在守护进程崩溃或机器重启后仍然存活。

## 开始之前

确保已配置好模型、已有可用的 Agent，且该 Agent 具有目标项目的访问权限。若模型配置尚未完成，可运行 `holon config doctor` 检查。下文中的 Shell 命令由操作者在终端执行；JSON 代码块则代表 Agent 发起的工具调用。

## 端到端工作流程

### 1. 启动守护进程

```bash
holon daemon start
holon daemon status
```

待守护进程报告运行中后再继续。若启动失败，请查阅[故障排查](troubleshooting.md#daemon-问题)。

### 2. 创建专用 Agent

若已有合适 Agent，可直接复用。若需新建，可在安装好 `software-developer` 模板后执行：

```bash
holon agent create builder --template software-developer
holon agent status builder
```

在授权修改文件前，确认 `builder` 已附加了预期的工作区。单纯创建 Agent 不会自动为你的任务关联代码仓库。

### 快速验证：先用只读任务验证断线重连

在授权执行代码修改之前，建议先用一项无副作用的只读任务验证后台执行与断线重连能力：

```bash
holon prompt --agent builder \
  "检查当前项目的运行时环境与工具链版本。将结果记录在新的 WorkItem 中并在完成后汇报。"
```

记录输出中返回的 `WORK_ITEM_ID`，随后即可直接断开连接（关闭终端窗口或退出 TUI）。守护进程会在后台继续推进工作。稍后重新打开终端核验：

```bash
holon work-item get WORK_ITEM_ID --agent builder
holon tail --agent builder --limit 10
```

核实该工作项保持了相同的目标标识，且包含了断开后新生成的工作简报。确认机制运转正常后，再继续下方的完整多会话代码修改流程。

### 3. 启动由 WorkItem 支持的工作

向该 Agent 提交清晰的目标与验收边界：

```bash
holon prompt --agent builder   "以 WorkItem 形式跟踪改进当前附加项目的错误处理逻辑。检查代码并编写包含验收项的计划。在编辑前请我确认批准计划。不要执行 git push，也不要提 PR。"
```

Agent 在新建前会先检查是否已有适用工作项。它会在计划中记录范围、验收检查项与待决策事项，并在待办清单（todo_list）中记录进度。查阅运行时返回的计划保存路径，在范围完全清晰后再行批准：

```bash
holon prompt --agent builder   "计划已批准。请在当前 WorkItem 中继续推进，完成范围内的代码修改并执行验收检查。"
```

Agent 会将 `plan_status` 更新为 `ready` 并继续推进该工作项。批准计划并不等同于该工作项已经完成。

### 4. 从其他会话中查看进度

```bash
holon agent status builder
holon work-item list --agent builder
holon work-item get WORK_ITEM_ID --agent builder
holon tail --agent builder --limit 10
```

将 `WORK_ITEM_ID` 替换为列表命令返回的实际 ID。在发送进一步指令前，核对目标、计划与检查清单。当前工作项代表 Agent 当前的注意力焦点；它可能正处于等待中或出让给了另一个工作项。

### 5. 等待外部事件

#### 等待命令执行完成

耗时极短的命令通常会立即返回。当命令转为后台任务时，Agent 会保存返回的任务 ID 并调用 `WaitFor` 进行等待：

```json
{
  "wake": "task_result",
  "resource": "TASK_ID_FROM_EXEC_COMMAND",
  "reason": "等待验收测试结果",
  "delivery": "silent"
}
```

Agent 会遵循返回的 `continuation`：如果结果当前已就绪，可能会在同一轮次中立即继续。任务所有权严格归属于创建它的 WorkItem；切换焦点或从其他工作项发起等待不会转移该任务的所有权。

#### 等待操作者输入

当需要操作者决策时，Agent 会记录具体问题，适时将 `plan_status` 置为 `needs_input`，并调用 `WaitFor(wake=operator_input)`，注明等待原因与显式交付模式。你后续可通过 `holon prompt --agent builder` 给出回复。Agent 恢复执行时会重新核对决策并更新同一份计划。

#### 等待外部触发器

针对 CI 构建或 PR 审查，Agent 会记录外部对象并登记等待：

```json
{
  "wake": "external",
  "resource": "github:owner/repo#123",
  "reason": "在评估改动前等待 CI 执行完毕",
  "recheck_after_ms": 600000,
  "delivery": "final"
}
```

在 `delivery=final` 模式下，出让控制权需要输出非空的等待简报。`WaitFor` 不会自动配置 Webhook；请参阅[连接外部事件](connect-external-events.md)配置实际推送。被唤醒后，Agent 会主动查询 CI 的真实状态再做决策。唤醒本身不证明依赖已经通过，也不构成自动合并的授权。

### 6. 处理执行中断

| 中断场景 | 恢复检查动作 |
|---|---|
| 终端或 TUI 断开连接 | 重新连接并检查该 Agent 及工作项。后台守护进程可能仍在正常推进。 |
| 守护进程重启 | 在恢复工作前，读取持久化的计划、就绪状态、等待状态与任务状态。 |
| 机器重启 | 启动守护进程，检查哪些命令需要重新执行。切勿假设之前的进程依然存活。 |
| 回调丢失或 CI 状态陈旧 | 重新查询外部源状态；在重试前核查等待与回调配置。 |

切勿因为客户端断开连接就重复创建新的工作项。如果某个工作项处于 `yielded`（已出让）状态，应检查其出让的目标项；严禁为了强制恢复而手动清除阻塞标记。

### 7. 接收最终交付简报

验收检查全部通过后，Agent 会调用 `CompleteWorkItem` 并提供非空的完成报告。重新拉取该工作项并确认：

- 其状态处于 `completed`；
- `result_brief_id` 指向对应的交付简报；
- 报告中明确列出了修改点、验证结论以及剩余的限制说明。

除了阅读报告外，务必核查实际的 Diff 或产物文件。后台命令执行成功仅仅是达成目标的一项支撑证据。

## 适用场景判断

在处理需要跟踪跨会话进展和等待外部依赖的任务时使用该工作流。对于能在一轮内完成的简短提问或代码排查，直接提问即可，无需创建 WorkItem。有关周期性执行的任务，请参阅[调度 Agent 工作](schedule-agent-work.md)。

## 另见

- [工作项参考](../reference/work-items.md)
- [观察 Agent 工作](observe-agent-work.md)
- [委派工作](delegate-work.md)
- [上下文连续性](../concepts/context-continuity.md)
