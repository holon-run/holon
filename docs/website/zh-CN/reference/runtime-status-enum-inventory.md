---
title: 运行时状态枚举清单
summary: 稳定序列化运行时生命周期和状态枚举的机器可读基线。
order: 27
---

# 运行时状态枚举清单

Holon 稳定的运行时生命周期标签由带类型的 Rust 枚举生成，并检入为
[`runtime-status-enum-inventory.json`](/reference/runtime-status-enum-inventory.json)。

- **权威来源：** 具名 Rust 枚举定义及其 serde rename 属性。
- **生成的清单：**
  `holon::contract_inventory::runtime_status_enum_inventory()`。
- **漂移检查：** `make snapshots-check`。
- **刷新：** 执行 `make snapshots-refresh`，然后审阅生成的 JSON diff。

## 当前覆盖范围

首个基线覆盖：

- `AgentStatus`
- `WorkItemState`
- `WorkItemPlanStatus`
- `WorkItemReadiness`
- `TaskStatus`
- `WaitConditionStatus`
- `TimerStatus`
- `QueueEntryStatus`
- `ToolResultStatus`

校验的是序列化后的 snake_case 契约，而不是 Rust variant 的拼写。新增或删除
variant，或修改其序列化名称，都会让快照检查失败，直到有意识地刷新该变更。

这份清单刻意保持狭窄。它不会把每个内部枚举都判定为稳定，不解析 Rust 源码文本，
也不把仅有散文描述的状态变成机器契约。
