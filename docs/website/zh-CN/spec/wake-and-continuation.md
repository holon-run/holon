---
title: 唤醒与延续
summary: 当前触发器分类、外部 ingress 能力、延续解析和 wake/sleep 生命周期。
order: 40
---

# 唤醒与延续

本页定义 Holon Agent 如何从休眠中唤醒、接收外部事件并解析延续决策的当前契约。

> **Last verified:** 2026-09-13 against `src/types.rs`
> `ContinuationTriggerKind`, `ContinuationClass`, `ContinuationResolution`,
> `PendingWakeHint`, `ExternalTriggerScope`, `CallbackDeliveryMode`,
> `ExternalTriggerSummary`, `ExternalTriggerCapability`,
> `WaitConditionRecord`, and `src/runtime/waiting.rs`.

## 源 RFC

- [延续触发器](https://github.com/holon-run/holon/blob/main/docs/rfcs/continuation-trigger.md)
- [外部触发能力与无 Provider 入站](https://github.com/holon-run/holon/blob/main/docs/rfcs/external-trigger-capability.md)
- [等待平面与重新激活](https://github.com/holon-run/holon/blob/main/docs/rfcs/waiting-plane-and-reactivation.md)
- [操作者等待与介入](https://github.com/holon-run/holon/blob/main/docs/rfcs/operator-wait-and-intervention.md)
- [远程操作者传输与投递](https://github.com/holon-run/holon/blob/main/docs/rfcs/remote-operator-transport-and-delivery.md)
- [事件流接口设计](https://github.com/holon-run/holon/blob/main/docs/rfcs/event-stream-interface.md)

## 触发器分类

Agent 被重新激活时，运行时按触发类型和类别对这次延续分类：

### 触发类型（`ContinuationTriggerKind`）

| 类型 | 来源 |
|------|--------|
| `OperatorInput` | 操作者直接消息（CLI、HTTP、TUI） |
| `TaskResult` | 命令任务或子 Agent 任务进入终态 |
| `ExternalEvent` | 外部系统通过 ingress URL 发来带内容的事件 |
| `TimerFire` | 计划中的定时器到时 |
| `InternalFollowup` | Agent 通过 `Enqueue` 给自己排了一条后续消息 |
| `SystemTick` | 调度器为可运行 WorkItem 发出运行时自有的后续消息 |

`SystemTick` 也是 Holon 在晋升的 `CompleteWorkItem` 报告结束当前轮次后恢复其他
可运行 WorkItem 的方式。

### 延续类别（`ContinuationClass`）

| 类别 | 含义 |
|-------|---------|
| `ResumeExpectedWait` | 触发器与先前的等待原因完全匹配 |
| `ResumeOverride` | 触发器覆盖了先前的等待（例如操作者打断） |
| `LocalContinuation` | Agent 未休眠即继续（同轮次后续） |
| `TaskResultReentry` | 终态任务结果重新进入同一个 WorkItem，而先前轮次并未在等待它 |
| `LivenessOnly` | 唤醒提示：不携带模型可见内容 |

## 唤醒提示与带内容事件

**唤醒提示**是活性信号。它们告诉调度器“外部有变化，重新评估 Agent 是否该唤醒”。
提示载荷**不会**作为模型可见消息投递。Agent 唤醒后必须自行向外部系统查询细节。

**带内容的外部事件**使用 `enqueue_message` 投递模式。事件载荷作为一条消息进入
Agent 队列，并保留来源信息。

| 投递模式 | 行为 |
|---------------|----------|
| `WakeHint` | 活性信号；调度器发出 `EmitSystemTick` |
| `EnqueueMessage` | 完整事件载荷作为消息入队 |

## 外部触发能力

Holon 在初始化时为每个 Agent 分配一个**默认外部 ingress 能力**。这是一个带密钥
token 的能力 URL：

- URL 作为 `default_external_ingress` 暴露在 Agent 的上下文中。
- 外部系统向该 URL POST，以投递唤醒提示或事件。
- 该能力以 Agent 为范围；可以跨 WorkItem 复用。
- 撤销能力是管理动作，不是按周期调用的模型工具。

### 能力模型

| 字段 | 用途 |
|-------|---------|
| `external_trigger_id` | 唯一的触发器标识 |
| `trigger_url` | ingress URL（能力密钥） |
| `target_agent_id` | 该触发器唤醒的 Agent |
| `delivery_mode` | `WakeHint` 或 `EnqueueMessage` |
| `scope` | `Agent` |
| `status` | `Active` 或 `Revoked` |

外部触发器在 Agent 初始化时分配。Agent 不会在运行时创建或撤销触发器。

## 延续解析

Agent 唤醒时，`ContinuationResolution` 记录这次激活是怎么发生的：

| 字段 | 含义 |
|-------|---------|
| `trigger_kind` | 导致唤醒的事件类型 |
| `class` | 这次唤醒与先前等待的关系 |
| `model_reentry` | 是否应带上下文重新进入模型 |
| `prior_closure_outcome` | 先前轮次的决定 |
| `prior_waiting_reason` | Agent 当时在等待什么 |
| `matched_waiting_reason` | 触发器是否匹配该等待原因 |

**关键契约：**

- `model_reentry=true` 表示模型会收到延续上下文，其中包含先前的轮次结束状态和
  触发器证据。
- `model_reentry=false` 表示这次唤醒是活性检查；调度器会重新评估姿态，但可能不
  需要重新进入模型。
- `matched_waiting_reason` 区分“预期唤醒”和“意外唤醒”。在等待任务结果时到达的
  操作者消息不匹配，Agent 可能需要处理这次打断。

## 已知缺口

- 等待状态由 `WaitConditionRecord.work_item_id` 划定范围：有值表示绑定 WorkItem
  的等待，`None` 表示 Agent 级等待。外部触发能力本身以 Agent 为范围，只按投递
  模式划分。
- `WakeHint` 的幂等性通过 `PendingWakeHint` 去重实现，但重复提示何时静默丢弃、
  何时作为诊断暴露，其契约还不是稳定 API。
- 外部事件的 `EnqueueMessage` 投递会保留来源信息，但外部来源信息的分类法尚未
  完全定义。
