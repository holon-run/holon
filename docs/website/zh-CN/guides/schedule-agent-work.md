---
title: 调度 Agent 工作
summary: 定时执行已授权的只读检查，验证返回的工作结果并及时取消定时器。
order: 110
---

# 调度 Agent 工作

定时器可在经过设定的延迟后或按固定周期唤醒 Agent。在创建定时器前，应先明确授权 Agent 可以执行哪些动作（如检查某次具体的 CI 构建状态），以及该调度何时应当终止。定时器的摘要说明仅用于备忘，并不会赋予额外的执行权限。

## 前置条件

使用一个运行中的守护进程以及具备正常模型连接的现有 Agent（本示例使用 `builder`）。先与 Agent 达成一项无害的第一步行动约定，例如读取某次已知 CI 构建的状态并输出汇报，严禁私自修改或提交代码：

```bash
holon agent status builder
holon prompt --agent builder \
  "跟踪已约定的 CI 检查。当定时器触发时，读取该次构建的当前状态并进行汇报。不要推送代码、不要合并 PR，也不要启动新的构建。"
```

在配置调度前，先明确以下约定输入：

| 输入项 | 示例 | 说明 |
|---|---|---|
| 目标数据源 | 具体的 CI URL 或 run ID | 必须可在不额外申请鉴权的情况下只读访问 |
| 只读访问方式 | 已预设的 Token 或公开接口 | 严禁在提示词明文中粘贴机密 |
| 允许执行动作 | 读取状态并生成简要报告 | 不可修改代码、推送、合并或重启构建 |
| 停止触发条件 | 构建进入终态（如 `passed` 或 `failed`） | 达到最大检查次数或超时时也应停止 |
| 汇报交付位置 | 当前 WorkItem 的完成简报 | 便于跨会话查看追踪 |

等待 Agent 确认数据源与工作范围后，再配置定时调度。下方的 CLI 命令由操作者在终端执行；对应的 Agent 工具调用方式在后文单独说明。

## 1. 配置单次检查

```bash
holon timer create --agent builder --after-ms 60000 \
  --summary "单次检查约定的 CI 构建状态"
holon timer list --agent builder
```

保存返回的定时器 ID（`timer_id`）。核对列表中显示的归属 Agent、触发时间与摘要信息。创建定时器本身不会暂停 Agent 当前的对话轮次。

若 Agent 自身需要等待该定时器到期，它会调用 `CreateTimer` 工具：

```json
{ "duration_ms": 60000, "summary": "单次检查约定的 CI 构建状态" }
```

随后将**返回的**实际 `timer_id` 传递给 `WaitFor` 工具：

```json
{
  "wake": "timer",
  "resource": "TIMER_ID_FROM_CREATE_TIMER",
  "reason": "等待定时器到期以检查约定的 CI 状态",
  "delivery": "silent"
}
```

诸如 `"一分钟后"` 之类的自然语言描述不能作为 `resource`。Agent 定时器工具仅允许操作归属于该 Agent 自身的定时器。

## 2. 核对检查结果

定时器到期后：

```bash
holon timer list --agent builder
holon tail --agent builder --limit 10
holon work-item list --agent builder
```

同时核对定时器状态与产生的执行报告。核验成功代表 Agent 确实检查了约定源并输出了汇报。单纯定时器被触发并不代表模型调用一定成功，也不代表 CI 构建本身已经通过。

对照已约定的输入表核验以下结果项：

| 核验项 | 预期证据 |
|---|---|
| 来源与构建 ID | 与预先约定的目标构建完全一致 |
| 检查发生时间 | 发生在定时器计划到期时间点或紧随其后 |
| 观察到的状态 | 来自数据源的原始状态（如 `success`、`in_progress`） |
| 定时器状态 | 匹配保存的 ID，状态为已完成或显示下一次触发时间 |
| WorkItem 记录 | 已通过 `holon work-item get` 更新至 WorkItem 简报中 |

## 3. 仅在必要时重复

若已获授权每 10 分钟执行一次周期性巡检：

```bash
holon timer create --agent builder --after-ms 600000 --every-ms 600000 \
  --summary "持续复核约定的 CI 构建状态直至完成"
```

Agent 工具调用对应的参数为 `interval_ms`，而非 CLI 的 `every_ms`：

```json
{
  "duration_ms": 600000,
  "interval_ms": 600000,
  "summary": "持续复核约定的 CI 构建状态直至完成"
}
```

将定时器 ID 与停止条件记录在 WorkItem 中。通知 Agent 当该构建达到终态时主动取消定时器。对于事件驱动的更新，优先采用外部回调；若要在外部等待中设置一次性兜底超时，请使用 `WaitFor(recheck_after_ms=...)`，无需另起定时调度。

## 4. 取消并确认

```bash
holon timer cancel TIMER_ID --agent builder
holon timer list --agent builder
```

将 `TIMER_ID` 替换为先前保存的实际 ID。对应的 Agent 工具为 `CancelTimer({"timer_id":"..."})`。确认定时器已被标记为已取消，并核实后续不再产生新的周期触发。取消操作不会撤销先前触发已经开始执行的工作。

## 检查迟到或未触发时的排查

- 在创建替代定时器前，先检查守护进程与 Agent 状态。
- 核对定时器 ID、归属者以及已取消或已完成状态。切勿在 `WaitFor` 中引用属于其他 Agent 的定时器。
- 检查 WorkItem 和最近简报，排查模型 Provider 故障或未决决策。定时器唤醒不会自动清除业务阻塞。
- 服务重启后，检查持久化的定时器与任务状态。不可假设精确的物理时钟对齐，也不可假设被中断的外部命令能够自动存活。

## 另见

- [运行长生命周期任务](run-long-lived-task.md) — 跨会话跟踪目标
- [事件与定时器参考](/zh-CN/reference/events-and-timers.md)
- [连接外部事件](connect-external-events.md) — 通过 Webhook 唤醒 Agent
