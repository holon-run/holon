---
title: 事件与定时器
summary: 回调路由、交付模式、定时器字段与显式等待契约。
order: 39
---

# 事件与定时器

关于具体配置与验证步骤，请参阅[连接外部事件](../guides/connect-external-events.md)或[调度 Agent 工作](../guides/schedule-agent-work.md)。本页记录这些工作流背后的权威契约。

## 回调端点（Callback endpoints）

| 方法与路由 | 交付模式 |
|---|---|
| `POST /api/callbacks/wake/:callback_token` | `wake_hint`：唤醒 Agent 检查外部状态 |
| `POST /api/callbacks/enqueue/:callback_token` | `enqueue_message`：将外部回调内容作为消息入队 |
| `POST /api/control/agents/:agent_id/reset-callback` | 认证控制操作：吊销当前默认凭据并颁发新凭据 |

每个回调对应一种交付模式和一个 `trigger_url`。回调 Token 是解析到有效触发器与目标 Agent 的能力凭据（Capability secret）。无效的 Token、未处于活跃状态的身份以及不匹配的交付模式均会被系统拒绝。回调被成功接收并不能证明底层业务已顺利完成。

回调端点支持可选的请求体。对于唤醒模式（wake hint），空的 POST 请求即可完成触发；对于入队通知模式（enqueue message），JSON 请求体可包含 `text` 字段：

```json
{ "text": "登记的 CI 构建状态发生变更，请核实当前状态。" }
```

回调载荷始终属于外部证据。持有回调 URL 的发送方并不会因此获得操作者指令权威。回调端点的请求体限制为 256 KB；请传递稳定的来源标识，避免发送庞大的日志全文。

重置接口返回 `ok`、`agent_id`、`external_trigger_id`、`trigger_url`、`delivery_mode` 和 `status`。应将返回的 URL 视同机密并直接使用。切勿将其打印至对话记录、粘贴到 Issue 或提交到版本控制中。

## 默认接入凭据与吊销

Agent 的运行时上下文会自动注入其默认接入凭据。对于常规集成，直接复用该凭据即可，无需频繁创建多个触发器。`CreateExternalTrigger` 仅用于受支持的兼容模式或备用模式场景，并非绕过调用方权限的手段。

`CancelExternalTrigger` 接收 `external_trigger_id` 并吊销该能力。重置操作会使旧的默认 URL 立即失效并生成新凭据。停止 Agent 会自动吊销其所有外部触发器；当 Agent 重新启动时，不要假定外部发送方保存的历史密钥依然有效。

## 定时器操作

| 操作 | 操作者 CLI | Agent 工具 |
|---|---|---|
| 创建 | `holon timer create --after-ms N [--every-ms N] [--summary TEXT] [--agent ID]` 或 `holon timer create --calendar-expression "M H * * *" --timezone AREA/LOCATION` | `CreateTimer(duration_ms?, interval_ms?, calendar_expression?, timezone?, summary?)` |
| 列表 | `holon timer list [--limit N] [--agent ID]` | `ListTimers(limit?)` |
| 详情 | 见下方 HTTP 路由 | `GetTimer(timer_id)` |
| 取消 | `holon timer cancel TIMER_ID [--agent ID]` | `CancelTimer(timer_id)` |

系统依然支持旧版的 `holon timer --after-ms N` 创建语法。Agent 工具仅对当前 Agent 生效，不接受指定目标 Agent ID。延迟与周期参数均为正毫秒数。创建定时器不会暂停当前轮次。取消已处于取消状态的定时器是幂等的；已触发完成的单次定时器无法再被取消。

日历定时器接收严格的五段式标准 cron 字段（分、时、日、月、周）与 IANA 时区。表达式根据指定时区求值，而 `next_fire_at` 和持久化时间戳依然保留为 UTC。日历定时器在每次触发后以及系统重启后重新计算下一次匹配时间，不采用固定 UTC 间隔，因此能够准确处理夏令时（DST）切换。`calendar_expression` 与 `timezone` 必须成对提供，且不能与 `duration_ms` 或 `interval_ms` 混用。

| HTTP 路由 | 契约说明 |
|---|---|
| `POST /api/control/agents/:id/timers` | 创建定时器，接收 `duration_ms`/`interval_ms` 或 `calendar_expression`/`timezone`，以及可选 `summary` 与控制层 `authority_class` 元数据 |
| `GET /api/agents/:id/timers` | 列出近期的定时器记录 |
| `GET /api/agents/:id/timers/:timer_id` | 读取归属于该 Agent 的定时器详情，不存在时返回 404 |
| `POST /api/control/agents/:id/timers/:timer_id/cancel` | 取消定时器；支持可选的控制层 `authority_class` 元数据 |

控制层元数据不会越权提升调用方本身的权限。认证与准入规则请参阅 [HTTP 控制平面参考](http-control-plane.md)。

### 定时器记录字段

| 字段 | 含义 |
|---|---|
| `id`、`agent_id` | 定时器唯一标识与所属 Agent |
| `created_at` | 创建时间戳 |
| `duration_ms` | 首次触发延迟（毫秒） |
| `interval_ms` | 可选的重复周期（毫秒） |
| `repeat` | 是否周期性重复触发 |
| `status` | `active`、`completed` 或 `cancelled`；旧版 `scheduled` 会被反序列化为 `active` |
| `summary` | 可选的提醒说明文本 |
| `next_fire_at`、`last_fired_at` | 下次预计触发时间戳与最近一次已触发时间戳 |
| `calendar` | 日历定时器的归一化五段表达式与 IANA 时区配置 |
| `fire_count` | 已记录的触发次数 |

## WaitFor 契约

`WaitFor` 用于登记调度等待条件；它既不创建定时器，也不配置 Webhook。其字段定义如下：

| 字段 | 契约要求 |
|---|---|
| `wake` | `operator_input`、`task_result`、`external`、`timer` 或 `system` |
| `resource` | 任务结果对应任务 ID；定时器对应实际的定时器 ID；外部等待对应可选的稳定外部对象标识 |
| `reason` | 清晰阐明阻碍当前继续推进的具体原因 |
| `delivery` | 必填项：`final`（需要面向操作者的报告）或 `silent`（静默） |
| `work_item_id` | 可选项：当无绑定的执行项时，显式指定归属的已开启工作项 |
| `recheck_after_ms` | 可选项：回退重新核对的超时延迟，并非周期性调度 |

`wake=timer` 要求定时器归属于当前 Agent 且具备可用的唤醒信号。无效、归属于他人、已取消或已被消费的定时器资源会返回可恢复的工具错误。对于 `wake=task_result`，任务可以属于该 Agent 的任意工作项；等待操作绝不会迁移任务的归属。

若存在绑定的执行工作项，则该工作项具有等待所属权，即使传入了冲突的参数也会被忽略并如实记录。若无绑定，解析顺序依次为：显式指定的所属已开启工作项、轮次绑定的工作项、当前焦点，或回退至 Agent 生命周期级。

### 后续流转与交付模式

| 返回的延续状态 | Agent 应对动作 |
|---|---|
| `continue_turn` | 立即在当前轮次继续执行有效工作；无需发布等待报告 |
| `yield_and_wait` | 让出执行权，挂起直至注册的唤醒信号到达 |
| `yield_and_reenter` | 让出执行权，等待排队的确切结果重新进入模型 |

若选择 `delivery=final` 并返回了 `awaiting_final_report`，代表等待尚未最终提交。此时必须输出一段非空的操作者答复；在该报告后续轮次中，仅允许调用只读核验工具，严禁递归调用生命周期工具。若选择 `delivery=silent`，挂起时无需输出面向操作者的等待报告。

等待注册与最终报告提交是强原子绑定的。陈旧的执行上下文、工作项版本冲突、唤醒状态变更或并发插话均会导致预备的等待失效。即便工作项存在独立的业务阻塞，匹配成功的唤醒信号依然能够恢复后续执行，但这绝不代表满足或清除了该业务阻塞。停止、完成、替换、取消以及结果消费屏障依然能够防止过时或重复的延续执行。
