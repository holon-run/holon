---
title: 工具
summary: 当前面向模型的工具家族、权限边界、输入/结果契约和已废弃接口。
order: 60
---

# 工具

本页定义 Holon 面向模型的工具接口的当前契约：工具家族、权限分类、
schema/分发对齐，以及结果信封约定。

内置面向模型工具的机器可读清单是
[`model-tool-schema-inventory.json`](/zh-CN/reference/model-tool-schema-inventory.json)；
它的版本策略和刷新流程记录在
[模型工具 schema 清册参考](/zh-CN/reference/model-tool-schema-inventory.md)。

> **Last verified:** 2026-05-27 against `src/types.rs`
> `ToolCapabilityFamily`, `src/tool/tools/mod.rs` `builtin_tool_definitions()`,
> `src/tool/spec.rs`, `src/tool/dispatch.rs`.

## 源 RFC

- [工具接口分层](https://github.com/holon-run/holon/blob/main/docs/rfcs/tool-surface-layering.md)
- [工具契约一致性](https://github.com/holon-run/holon/blob/main/docs/rfcs/tool-contract-consistency.md)
- [工具结果信封](https://github.com/holon-run/holon/blob/main/docs/rfcs/tool-result-envelope.md)
- [任务接口收窄](https://github.com/holon-run/holon/blob/main/docs/rfcs/task-surface-narrowing.md)
- [命令工具家族](https://github.com/holon-run/holon/blob/main/docs/rfcs/command-tool-family.md)
- [Apply Patch 统一 Diff 契约](https://github.com/holon-run/holon/blob/main/docs/rfcs/apply-patch-unified-diff-contract.md)
- [Exec Command Batch](https://github.com/holon-run/holon/blob/main/docs/rfcs/exec-command-batch.md)

## 工具家族（`ToolCapabilityFamily`）

工具按能力家族分组，用于权限门控：

| 家族 | 工具 | 权限 |
|--------|-------|-----------|
| `CoreAgent` | `WaitFor`、`GetAgent`、`Enqueue`、`CreateTimer`、`ListTimers`、`GetTimer`、`CancelTimer`、`ListTasks`、`TaskStatus`、`TaskInput`、`TaskOutput`、`TaskStop`、`ListModelProviders`、`ListProviderModels`、工作项工具、`MemorySearch`、`MemoryGet` | 所有 Agent profile |
| `LocalEnvironment` | `ExecCommand`、`ExecCommandBatch`、`ApplyPatch`、`ViewImage`、`GenerateImage`、`GetWorkspaceState`、`SwitchWorkspace`、`CreateWorktree` | 所有 profile |
| `AuthorityExpanding` | `AttachWorkspace`、`DetachWorkspace`、`RemoveWorktree` | 公有具名 Agent |
| `Web` | `WebFetch`、`WebSearch`、`XSearch` | 所有 profile |
| `AgentCreation` | `CreateAgent`、`InvokeAgent` | 公有具名 Agent |
| `ExternalTrigger` | `CreateExternalTrigger`、`CancelExternalTrigger` | 所有 profile |

操作者通知记录、投递回调和 UI 渲染仍是运行时拥有的能力。它们不属于面向模型的
工具清单；`NotifyOperator` 有意不出现在内置工具注册表和机器可读 schema 清单中。
`ExternalTrigger` 工具目前遵循同样的模式：它们在注册表中可分发，但不在面向模型的
接口中。

## 完整工具列表

### 工作项平面

| 工具 | 用途 |
|------|---------|
| `CreateWorkItem` | 创建一个新的 open 工作项 |
| `UpdateWorkItem` | 修改 objective、plan_status、todo_list |
| `PickWorkItem` | 设置当前焦点 |
| `GetWorkItem` | 读取单个工作项，带计划预览 |
| `ListWorkItems` | 按过滤器查询 |
| `CompleteWorkItem` | 按 ID 完成一个自己拥有的目标；把同轮次的 assistant 文本提升为其规范完成报告 |
| `WaitFor` | 记录任务、外部或操作者等待状态并让出 |

### 任务控制平面

| 工具 | 用途 |
|------|---------|
| `ExecCommand` | 启动 shell 命令 |
| `ExecCommandBatch` | 运行有界的顺序命令批次 |
| `ListTasks` | 紧凑的活跃任务摘要，输出有界 |
| `TaskStatus` | 单任务生命周期快照 |
| `TaskOutput` | 有界输出预览，可选阻塞等待 |
| `TaskInput` | 向交互式任务发送输入 |
| `TaskStop` | 停止正在运行的任务 |

### Agent 平面

| 工具 | 用途 |
|------|---------|
| `GetAgent` | 读取当前 Agent 平面摘要 |
| `WaitFor` | 记录显式等待状态后发出轮次结束信号 |
| `Enqueue` | 安排自我后续消息 |
| `CreateAgent` | 创建一个长期、可寻址的 Agent |
| `InvokeAgent` | 通过父级监督的任务句柄委派工作 |

### 定时器平面

| 工具 | 用途 |
|------|---------|
| `CreateTimer` | 创建一个独立或重复的定时器 |
| `ListTimers` | 列出该 Agent 最近的定时器 |
| `GetTimer` | 按 id 读取一个定时器 |
| `CancelTimer` | 取消一个活跃定时器 |

### 模型平面

| 工具 | 用途 |
|------|---------|
| `ListModelProviders` | 列出已配置或已发现的模型提供商 |
| `ListProviderModels` | 列出某个提供商可选用的模型 |

### 图像平面

| 工具 | 用途 |
|------|---------|
| `GenerateImage` | 根据文本提示生成一张图像 |
| `ViewImage` | 校验本地图像并记录其元数据 |

### 工作区平面

| 工具 | 用途 |
|------|---------|
| `GetWorkspaceState` | 读取绑定、活跃投影、worktree 和占用情况 |
| `AttachWorkspace` | 附加工作区绑定但不切换 |
| `DetachWorkspace` | 解除绑定；活跃目标回退到 agent home |
| `SwitchWorkspace` | 激活已有的工作区或 execution root |
| `CreateWorktree` | 创建或安全复用链接的 worktree |
| `RemoveWorktree` | 安全移除已注册的干净 worktree |
| `ApplyPatch` | 对文件应用统一 diff 补丁 |

### 记忆平面

| 工具 | 用途 |
|------|---------|
| `MemorySearch` | 搜索 Agent 记忆来源 |
| `MemoryGet` | 按 source_ref 获取精确记忆内容 |

### Web 平面

| 工具 | 用途 |
|------|---------|
| `WebFetch` | 抓取 HTTP/HTTPS URL |
| `WebSearch` | 网络搜索 |
| `XSearch` | 搜索公开的 X 帖子 |

## 工具定义契约

每个工具由一个 `BuiltinToolDefinition` 定义：

```text
BuiltinToolDefinition {
    family: ToolCapabilityFamily,
    spec: ToolSpec { name, description, input_schema, freeform_grammar },
}
```

**关键契约：**

- 工具 schema 必须与用于解析参数的运行时类型一致。
- 工具参数结构体强制启用 `serde(deny_unknown_fields)`。
- `ToolSpec` 中的 `description` 字段是模型可见的指引文本。
- 提示层级的工具指引（在 AGENTS.md 或系统提示中）不得与工具自身的描述冲突。

## 输入与结果分离

Holon 严格区分工具的**启动输入**与**结果元数据**：

- 启动输入：`cmd`、`workdir`、`shell`、`login`、`tty`、`duplicate_policy`、
  `accepts_input`、`yield_time_ms`、`max_output_tokens`。
- 结果元数据（在启动输入中无效）：`status`、`task_handle`、`disposition`、
  `exit_status`、`output_preview`。

**关键契约：**

- 把结果字段当作启动输入传入是错误。
- 模型不得混淆两个接口。提示指引通过有效/无效启动示例明确记录这一区别。

## 结果信封

工具执行返回一个 `ToolResult`，可序列化为 JSON，或渲染为人类可读回执：

- **规范结果：** 结构化 JSON，包含 `content`（text/tool_use 块数组）和可选的
  `artifacts`。
- **人类可读回执：** 展示给模型的渲染文本；可以省略内部字段，但必须保留语义上
  重要的内容。

`ExecCommand` 结果带有额外字段：`disposition`、`exit_status`、
`initial_output_preview`，以及 `task_handle`（在提升为 command_task 时）。

## 已知缺口

- 工具描述文本在 Rust 源码中手工维护；没有自动化校验时，描述与实际行为可能漂移。
- `ExecCommandBatch` 和单次 `ExecCommand` 调用共享字段，但有效字段子集不同；
  类型层面没有结构化的 schema 阻止把仅属于批次的字段传给单次 `ExecCommand`。
