---
title: 信任与来源
summary: 当前 provenance、admission/authentication、指令权威和执行策略契约。
order: 80
---

# 信任与来源

本页定义 Holon 如何对消息来源、准入/认证、指令权威和执行策略进行分类，
以及这些标签如何流经运行时的当前契约。

> **Last verified:** 2026-09-13 against `src/types.rs` `MessageEnvelope`,
> `MessageOrigin`, `AuthorityClass`, `MessageDeliverySurface`,
> `AdmissionContext`, `src/policy.rs`, `src/ingress.rs`, `src/http/mod.rs`,
> `src/http/state.rs`, `src/context/mod.rs`, `src/context/render.rs`,
> `src/prompt/mod.rs`, `src/operator_event.rs`, `src/presentation.rs`,
> `src/runtime/message_dispatch.rs`, `src/runtime/operator_dispatch.rs`, and
> `src/runtime/turn/execution.rs`.

## 源 RFC

- [Provenance, Admission, and Authority](https://github.com/holon-run/holon/blob/main/docs/rfcs/default-trust-auth-and-control.md)
- [Event Stream Interface Design](https://github.com/holon-run/holon/blob/main/docs/rfcs/event-stream-interface.md)
- [Remote Operator Transport and Delivery](https://github.com/holon-run/holon/blob/main/docs/rfcs/remote-operator-transport-and-delivery.md)
- [Operator Display Levels and Event Presentation](https://github.com/holon-run/holon/blob/main/docs/rfcs/operator-display-levels-and-event-presentation.md)
- [Tool Surface Layering](https://github.com/holon-run/holon/blob/main/docs/rfcs/tool-surface-layering.md)
- [Continuation Trigger](https://github.com/holon-run/holon/blob/main/docs/rfcs/continuation-trigger.md)

## 消息信封

Holon 中每条排队消息都在 `MessageEnvelope` 中携带来源、准入、权威和调度标签：

```text
MessageEnvelope {
    origin: MessageOrigin,
    authority_class: AuthorityClass,
    priority: Priority,
    delivery_surface: Option<MessageDeliverySurface>,
    admission_context: Option<AdmissionContext>,
    trigger_kind: Option<ContinuationTriggerKind>,
    source_refs: BTreeMap<String, String>,
    ...
}
```

下面四个概念有意保持分离：

- **来源（provenance）** 回答内容由谁或什么产生（`origin`），并保留关联的来源标识符
  （`source_refs`）。
- **准入/认证** 回答 Holon 如何以及为何接受该消息（`delivery_surface` 和
  `admission_context`）。
- **指令权威** 回答内容是操作者指令、运行时指令、集成信号还是外部证据（`authority_class`）。
- **执行策略** 回答当前执行边界内是否允许某个具体工具或资源操作。它消费这些标签，
  但不被任何单一标签取代。

## 来源：`MessageOrigin` 与 source refs

`MessageOrigin` 记录内容的生产者。它不编码入口如何认证，也不单独授予工具权限。

| Origin 变体 | 当前含义 | 典型 kind |
|----------------|-----------------|--------------|
| `Operator { actor_id }` | 操作者直接撰写的内容。准入表面可以是本地 CLI、run-once、HTTP control 或远程操作者传输。 | `OperatorPrompt`、`Control` |
| `Channel { channel_id, sender_id }` | 外部渠道内容。公共 enqueue 默认将其作为外部证据接受。 | `ChannelEvent` |
| `Webhook { source, event_type }` | 外部 webhook 内容。未提供 origin 时，公共 enqueue 默认使用该来源。 | `WebhookEvent` |
| `Callback { descriptor_id, source }` | 由 capability secret 准入的外部触发器回调。正文是集成信号，不是操作者指令。 | `CallbackEvent` |
| `Timer { timer_id }` | 定时器按计划触发。 | `TimerTick` |
| `System { subsystem }` | 运行时拥有的内部消息，例如 scheduler、lifecycle 或 internal follow-up。 | `SystemTick`、`InternalFollowup`、`Control` |
| `Task { task_id }` | 来自被监督命令或子 Agent 的任务状态/结果。 | `TaskStatus`、`TaskResult` |

`MessageEnvelope::normalize_admission_fields` 推导 `trigger_kind`、任务来源消息的
`task_id`，以及 `source_refs`，例如 `task_id`、`task_result_id`、`timer_id`、
`external_trigger_id`、`wait_id`、`wait_generation`、`callback_delivery_id` 和
`queued_event_id`。`work_item_id`、`task_id` 这类绑定字段只会从经由 `RuntimeSystem`
或 `TaskRejoin` 准入的运行时拥有消息的 metadata 中投影；不可信的外部 metadata 仍只是证据。

## 准入/认证：投递表面与准入上下文

`MessageDeliverySurface` 记录消息从何处进入运行时，或由运行时何处产生：

| 投递表面 | 当前用途 |
|------------------|-------------|
| `CliPrompt` | 本地交互式提示输入。 |
| `RunOnce` | 本地一次性运行输入。 |
| `HttpPublicEnqueue` | 远程访问准入后的公共 HTTP enqueue；不能请求 `Interject` 优先级，也不能覆盖权威。 |
| `HttpWebhook` | HTTP webhook 传输。 |
| `HttpCallbackEnqueue` | 将消息入队的 callback 端点。 |
| `HttpCallbackWake` | 用作 wake hint 的 callback 端点。 |
| `HttpControlPrompt` | 已认证的 HTTP control prompt。 |
| `RemoteOperatorTransport` | 已认证的远程操作者传输。 |
| `TimerScheduler` | 运行时定时器调度器。 |
| `RuntimeSystem` | 运行时拥有的系统表面。 |
| `TaskRejoin` | 任务监督器结果/状态 rejoin。 |

`AdmissionContext` 记录 Holon 为何接受该入口：

| 准入上下文 | 当前含义 |
|-------------------|-----------------|
| `PublicUnauthenticated` | 未使用操作者凭据即准入的公共 enqueue/webhook 式输入。 |
| `ControlAuthenticated` | 由配置的 control token 准入的控制平面请求。 |
| `OperatorTransportAuthenticated` | 作为操作者表面通过认证的远程操作者传输。 |
| `ExternalTriggerCapability` | 凭持有外部触发器 capability secret 而准入的 callback。 |
| `LocalProcess` | 本地进程或本地控制表面；当前 host-local 执行策略仍然适用。 |
| `RuntimeOwned` | 由运行时自身产生的消息。 |

准入不等同于指令权威。例如，`ExternalTriggerCapability` 证明某个 callback URL 有效，
但 callback 载荷仍是 `IntegrationSignal`，不是 `OperatorInstruction`。

## 权威类别（`AuthorityClass`）

`AuthorityClass` 是当前的指令权威词汇表：

| 类别 | 含义 | 默认来源 |
|-------|---------|-----------------|
| `OperatorInstruction` | 操作者撰写、Agent 应遵循的指令，但受指令优先级和执行策略约束。 | `Operator` |
| `RuntimeInstruction` | 运行时拥有的指令或生命周期/任务信号。 | `System`、`Task`、`Timer` |
| `IntegrationSignal` | 已配置的集成或 callback 信号。它可以唤醒或告知工作，但不是操作者指令。 | `Webhook`、`Callback` |
| `ExternalEvidence` | 供检查的外部渠道内容。 | `Channel` |

**关键契约：**

- `AuthorityClass` 由入口/运行时代码赋值，模型不会重新赋值。
- 公共 enqueue 不能覆盖 `authority_class`；可信入口可以提供它，或按 origin 取默认值。
- `validate_message_kind_for_origin` 只准入符合运行时契约的 origin/kind 组合。
- 合并操作者输入与外部渠道输入时，必须保留来源。
- prompt 要求模型把外部或低权威载荷当作证据，而不是等同于操作者指令。

### 过渡期的 `trust` 措辞

早期的 `trust` / `trusted_*` / `untrusted_*` 词汇不是主要的公开契约。当前代码在两处保留兼容：

- `MessageEnvelope` 反序列化接受旧版 `trust` 作为 `authority_class` 的别名。
- `AuthorityClass` 变体接受旧的 serde/CLI 别名，例如 `trusted_operator`、
  `trusted_system`、`trusted_integration` 和 `untrusted_external`。

[`src/context/render.rs`](../../../src/context/render.rs) 保留了一个旧版 `trust_label`
映射，把 `authority_class` 映到旧的 `trusted_*` / `untrusted_*` 名称；但当前模型上下文
的消息头用 `authority_class_label`（`operator_instruction`、`runtime_instruction` 等）
标注消息。新文档和新契约应直接使用 `authority_class`。

## 当前分类矩阵

| 生产者/路径 | Origin | Kind | 权威 | 投递表面 | 准入上下文 |
|-----------------|--------|------|-----------|------------------|-------------------|
| 本地操作者提示 | `Operator` | `OperatorPrompt` | `OperatorInstruction` | `CliPrompt` / `RunOnce` | `LocalProcess` |
| 带 token 的 HTTP control prompt | `Operator` | `OperatorPrompt` 或 `Control` | `OperatorInstruction` | `HttpControlPrompt` | `ControlAuthenticated` |
| 远程操作者传输 | `Operator` | `OperatorPrompt` | `OperatorInstruction` | `RemoteOperatorTransport` | `OperatorTransportAuthenticated` |
| 公共外部渠道 enqueue | `Channel` | `ChannelEvent` | `ExternalEvidence` | `HttpPublicEnqueue` | `PublicUnauthenticated` |
| 公共 webhook enqueue | `Webhook` | `WebhookEvent` | `IntegrationSignal` | `HttpPublicEnqueue` / `HttpWebhook` | `PublicUnauthenticated` |
| 外部 callback enqueue/wake | `Callback` | `CallbackEvent` | `IntegrationSignal` | `HttpCallbackEnqueue` / `HttpCallbackWake` | `ExternalTriggerCapability` |
| 定时器触发 | `Timer` | `TimerTick` | `RuntimeInstruction` | `TimerScheduler` | `RuntimeOwned` |
| 运行时系统/internal follow-up | `System` | `SystemTick` / `InternalFollowup` / `Control` | `RuntimeInstruction` | `RuntimeSystem` | `RuntimeOwned` |
| 任务状态/结果 | `Task` | `TaskStatus` / `TaskResult` | `RuntimeInstruction` | `TaskRejoin` | `RuntimeOwned` |

## 优先级

| 优先级 | 调度效果 |
|----------|-------------------|
| `Interject` | 抢占排队工作；在普通优先级消息之前投递 |
| `Next` | 在当前 interject 消息之后、`Normal` 之前投递 |
| `Normal` | 标准队列位置 |
| `Background` | 低紧急度；在更高优先级消息之后投递 |

优先级影响队列顺序，不影响权威、准入或执行策略。

## 执行策略

执行策略是具体进程、文件、网络、消息入口、控制平面、workspace 投影和 Agent 状态
操作的最终允许/拒绝边界。它以当前执行策略快照的形式向模型汇总，并在存在硬强制
的地方由运行时/工具表面执行。

权威标签为执行策略提供信息，但不取代它。例如，`OperatorInstruction` 可以请求某个
操作，但工具仍会在当前 workspace 投影、进程执行、密钥隔离以及路径/写入/网络策略
下运行。反过来，`RuntimeInstruction` 可以携带生命周期状态，但这不会让任意外部
载荷文本变得可信。

## 来源保留与暴露

Holon 的来源契约：

- Origin、权威、投递表面、准入上下文、trigger kind、source refs、关联 id 和因果 id
  **永远不会**被模型重新赋值。
- 当运行时生成内部消息（system tick、task result、timer fire、continuation
  follow-up）时，它会赋予运行时拥有的 origin 和 `RuntimeInstruction` 权威。
- 当 Agent 通过 `InvokeAgent` 委派工作时，被调用的 Agent 会按照监督运行时表面的
  规则，把委派任务作为有界的操作者/运行时上下文接收；它不得把后续外部渠道内容
  静默合并进操作者指令。
- 模型上下文渲染当前消息的 origin、权威类别、投递表面、admission context、
  trigger kind、绑定 id 和 message kind。操作者 interjection 会在 turn prompt 中
  携带显式的 `origin`、`authority_class`、`delivery_surface` 和
  `admission_context` 元数据。
- TUI 和一方展现使用原始 projection/runtime 事件，并在客户端归约。用户消息展现只
  渲染 origin 为 `Operator` 的消息；外部事件不会仅因包含文本就变成用户聊天消息。
- HTTP 事件流暴露原始运行时事件和消息载荷，包括消息准入、处理和 transcript 事件上
  的来源标签。
- 面向用户的摘要应概括结果、阻塞和需要操作者执行的动作，同时不丢弃持久的
  运行时/事件流中底层消息/事件的来源信息。

## 验证

已验证的实现要点：

- `src/types.rs` 定义规范的信封字段和旧版别名兼容。
- `src/policy.rs` 定义按 origin 的默认权威以及 kind/origin 准入检查。
- [`src/http/state.rs`](../../../src/http/state.rs) 阻止公共 enqueue 使用运行时拥有的
  kind、`Interject` 优先级、特权 origin 或权威覆盖。
- [`src/context/mod.rs`](../../../src/context/mod.rs) 和
  [`src/prompt/mod.rs`](../../../src/prompt/mod.rs) 向模型暴露权威/来源标签，并在
  prompt 指令中保留外部证据的可信边界。
- [`src/runtime/message_dispatch.rs`](../../../src/runtime/message_dispatch.rs)、
  [`src/runtime/operator_dispatch.rs`](../../../src/runtime/operator_dispatch.rs)
  和 [`src/runtime/turn/execution.rs`](../../../src/runtime/turn/execution.rs) 在队列、
  处理、transcript 和 interjection 事件中包含来源标签。
- `src/operator_event.rs` 和 `src/presentation.rs` 在把操作者来源消息渲染为用户消息的
  同时，保留原始事件来源信息。

## 漂移与后续分类

- **过时的 RFC 措辞：** `docs/rfcs/default-trust-auth-and-control.md` 称 `TrustLevel`
  可以作为过渡期实现细节保留。当前实现已经从公开的 `MessageEnvelope` 中移除
  `TrustLevel` 枚举/字段；现在只剩旧版 serde/CLI 别名。
- **未定的设计决策：** `signed_integration` 在 RFC 中作为可能的 admission context
  出现，但目前还没有 `AdmissionContext::SignedIntegration` 变体。
- **本页补齐的缺失测试覆盖：** `src/policy.rs` 现在有一个直接的分类矩阵测试，覆盖
  所有当前的 origin 默认值和允许的 origin/kind 组合。
- **未发现实现缺陷：** 所检查的入口、prompt、事件和展现路径均保持了当前的
  来源/权威分离。
