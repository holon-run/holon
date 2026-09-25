---
title: 模型工具 schema 清单
summary: Holon 面向模型的内置工具 schema、结果信封和稳定性标签的版本化清单。
order: 26
---

# 模型工具 schema 清单

本页定义 Holon **面向模型的内置工具接口**的版本策略。机器可读清单检入为
[`model-tool-schema-inventory.json`](/reference/model-tool-schema-inventory.json)。

- **权威来源：** `src/tool/tools/mod.rs` 的 `builtin_tool_definitions()`，以及
  派生 `schemars::JsonSchema` 的带类型 Rust 参数结构体。
- **生成的清单：** `holon::tool::model_tool_schema_inventory()`。
- **漂移检查：** `make snapshots-check`。
- **当前状态：** 1.0 之前的基线。把 stable 标签视为当前轨道打算保持的兼容边界，
  而不是最终的 1.0 承诺。

## 清单内容

每个内置工具条目记录：

- 工具名称
- 能力家族
- 稳定性级别
- 面向模型的输入 schema
- 工具接受非 JSON 输入时的自由格式语法
- 结果信封家族和模型渲染契约
- 当该稳定结果接口属于当前覆盖范围时，带类型的成功结果 JSON Schema
- 当命令是对工具或运行时 API 的封装时，相关的 HTTP 或 CLI 接口
- 模型可见的工具描述

版本 2 清单当前覆盖以下工具的结果 schema：`CreateTimer`、`ListTimers`、
`GetTimer`、`CancelTimer`、`Enqueue`、`CreateAgent`、
`SendAgentMessage`、`InvokeAgent`、`ListTasks`、`TaskStatus`、
`TaskInput`、`TaskOutput`、`TaskStop` 和 `GenerateImage`。未覆盖的工具保留其
结果类型名和显式的 `null` schema；这样可以避免把推断出的或不完整的形状当作
稳定契约。只有当具体 Rust 结果类型派生 `schemars::JsonSchema` 时，覆盖范围
才能扩展。

## 能力家族

每个内置工具都属于一个能力家族。运行时在 `src/types.rs`
（`ToolCapabilityFamily`）中定义六个家族：

| 家族 | 说明 | 示例工具 |
|--------|-------------|---------------|
| `CoreAgent` | 核心 agent 操作（状态、消息、记忆、工作项、调度、CLI/配置内省） | SendAgentMessage, MemorySearch, WaitFor, ListWorkItems, AdvisoryDecision |
| `LocalEnvironment` | 工作区本地操作 | ExecCommand, ApplyPatch, ViewImage, GetWorkspaceState, SwitchWorkspace, CreateWorktree |
| `Web` | 公共 Web 访问 | WebFetch, WebSearch |
| `AgentCreation` | agent 创建与受监督调用 | CreateAgent, InvokeAgent |
| `AuthorityExpanding` | 改变工作区权威或销毁已注册产物的工具 | AttachWorkspace, DetachWorkspace, RemoveWorktree |
| `ExternalTrigger` | 外部事件入口 | CreateExternalTrigger, CancelExternalTrigger |

## 稳定性级别

| 级别 | 含义 |
|-------|---------|
| `stable` | 名称、输入 schema、结果信封家族和已记录的模型渲染都保持兼容。 |
| `experimental` | 接口可用，但运行时契约尚未定型，仍可能变化。 |
| `deprecated` | 接口为兼容而保留，但不应引入新工作流。 |

## 命名策略

Holon 原生内置工具名使用 PascalCase，并以动作开头。集合读取用 `List*`，
单资源或快照读取用 `Get*`，控制或变更用明确动词，比如 `Send*`、`Stop*`、
`Create*`、`Update*`。迁移窗口期内调度器可能仍接受旧别名，但检入的清单对外
公布的是规范的、面向模型的名称。

## 版本策略

顶层 `version` 字段为清单格式定版本，而不是为每个工具 schema 单独定版本。

- 当清单文件形状的变化需要读取方区别处理时，递增 `version`。
- 普通的工具增删、输入 schema 变更、描述变更或稳定性标签变更都不递增
  `version`；这些是同一清单格式内的契约变更，通过快照 diff 审阅。
- 保留 Rust 定义作为权威来源。有意的变更应先更新 Rust，再刷新检入的清单。

## 刷新流程

```bash
make snapshots-refresh
make snapshots-check
```
