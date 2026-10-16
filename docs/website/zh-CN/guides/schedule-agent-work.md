---
title: 调度 Agent 工作
summary: 创建单次或周期性定时器，验证定时触发的工作并及时取消调度。
order: 110
---

# 调度 Agent 工作

定时器可在经过设定的延迟后或按固定周期唤醒 Agent。在创建定时器前，应先明确授权 Agent 可以执行哪些动作，以及该调度何时应当终止。定时器的摘要说明仅用于备忘，并不会赋予额外的执行权限。

## 前置条件

使用一个运行中的守护进程以及具备正常模型连接的现有 Agent（本示例使用 `builder`）。先与 Agent 达成一项无害的第一步行动约定，例如读取某次已知 CI 构建的状态并输出汇报，严禁私自修改或提交代码：

```bash
holon agent status builder
holon prompt --agent builder \
  "跟踪已约定的 CI 检查。当定时器触发时，读取该次构建的当前状态并进行汇报。不要推送代码、不要合并 PR，也不要启动新的构建。"
```

等待 Agent 确认数据源与工作范围后，再配置定时调度。下方的 CLI 命令由操作者在终端执行；对应的 Agent 工具调用方式在后文单独说明。

## 操作步骤

1. 创建定时器：

   ```bash
   # 创建单次定时器（例如 5 分钟后触发）：
   holon timer create --after-ms 300000 --summary "检查 CI 构建状态" --agent builder

   # 或创建周期性定时器（例如每 10 分钟触发一次）：
   holon timer create --after-ms 600000 --every-ms 600000 --summary "定期巡检构建状态" --agent builder
   ```

2. 检查定时器列表：

   ```bash
   holon timer list --agent builder
   ```

   记录输出中的 `TIMER_ID`。

3. Agent 侧的等待登记：
   若 Agent 当前必须等待定时器触发才继续推进，它会在底层调用 `WaitFor` 工具：

   ```json
   {
     "wake": "timer",
     "resource": "timer_12345",
     "reason": "等待定时器到期以检查 CI 状态",
     "delivery": "final"
   }
   ```

4. 验证触发与唤醒：
   当定时器到期时，系统会自动唤醒 Agent。检查 Agent 状态与执行流水：

   ```bash
   holon transcript --agent builder --limit 10
   ```

   确认 Agent 确实是在定时器触发后才执行检查，并核实其行为严格受限于最初授权的范围。

5. 取消不再需要的定时器：
   当工作完成或不再需要监控时，必须及时取消周期性定时器：

   ```bash
   holon timer cancel TIMER_ID --agent builder
   ```

## 另见

- [事件、等待与唤醒概念](/zh-CN/concepts/events-and-waits.md)
- [事件与定时器参考](/zh-CN/reference/events-and-timers.md)
- [连接外部事件](connect-external-events.md)
