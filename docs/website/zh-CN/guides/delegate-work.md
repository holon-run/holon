---
title: 将工作委派给另一个 Agent
summary: 挑选子 Agent 或对等 Agent、跟进其回复、验证工作结果并安全清理下属实例。
order: 80
---

# 将工作委派给另一个 Agent

将可独立验证的有界任务（例如只读审查一段 Diff）委派给专门的 Agent 执行。父 Agent 始终拥有最终验收责任并向操作者汇报。

## 开始之前

确保父 Agent 拥有目标工作区的访问权限以及明确的工作目标。确认父 Agent 具备调用 `InvokeAgent` 的权限。调用中指定的模板必须提前就绪；若调用已有对等 Agent，该目标必须真实存在且调用方有权访问。

下文中的 JSON 代码块代表 **Agent 工具调用入参**，并非 Shell 终端命令。操作者应提示父 Agent 执行这些步骤，切勿在外部通过递归执行 `holon run` 来模拟委派。

## 调用方式

### 受监督的子 Agent（Child Subagent）

适用于在父 Agent 监督下执行可分离的任务。调用 `InvokeAgent` 并声明工作范围、限制与返回格式：

```json
{
  "target": {
    "kind": "new_subagent",
    "template": "code-reviewer",
    "workspace_mode": "inherit"
  },
  "initial_message": "审查 src/runtime/ 下当前的 Diff。仅限只读检查；不要编辑、提交或发布任何内容。返回发现的问题、文件位置、严重级别以及未覆盖的验证缺口。若未发现问题，说明检查了哪些内容。"
}
```

妥善保存返回的 `agent_id` 与任务句柄（TaskHandle）。本只读示例继承了父工作区（`inherit`），以便子 Agent 能直接审查父工作区中未提交的 Diff。在审查期间请保持该 Diff 不被修改。

若委派涉及文件编辑任务，需使用 `workspace_mode: "worktree"` 并指定子 Agent 必须检查的具体提交版本或补丁。全新的独立 Worktree 默认**不会**隐式包含父工作区尚未提交的修改。Worktree 能够隔离代码仓库的编辑，但它并不是安全沙箱。必须明确告知子 Agent 允许修改哪些文件以及是否需要执行 Git 提交。

### 对等 Agent 调用（Peer Invocation）

当已有 Agent 拥有特定专业领域知识或长效职责时，采用对等调用：

```json
{
  "target": { "kind": "existing_agent", "agent_id": "auditor" },
  "initial_message": "在不修改代码的前提下审计当前的保留策略变更。回复审计发现以及检查的提交或 Diff；将确认收到的回执与完成审计的结论明确区分开。"
}
```

对等调用的配置与父子层级关系保持不变。其任务句柄在**首条后续持久化消息**到达时即宣告完成，而该消息可能仅仅是一条确认收到任务的回执。必须核验该回复是否真正满足了委派要求。多个并发等待可能会观察到同一条回复；它们并不各自代表一次独立的业务完成。

### 子 Agent Token 消耗

在排查开销或非预期任务量时，可通过任务快照审查 `token_usage`。字段定义见[委派任务参考](../reference/work-items.md#delegated-task-snapshots)。Token 消耗仅代表资源消耗量，并不等同于任务进度或正确性。

## 监督流程

1. 保存 `InvokeAgent` 返回的任务句柄。
2. 若有其他独立任务，父 Agent 可并行继续处理。在排查问题时，可使用 `TaskStatus` 或 `TaskOutput` 进行有界检查。
3. 当仅剩等待委派结果时，使用返回的任务 ID 调用 `WaitFor`：

   ```json
   {
     "wake": "task_result",
     "resource": "TASK_ID_FROM_INVOKE_AGENT",
     "reason": "等待审查结论返回",
     "delivery": "silent"
   }
   ```

4. 根据返回的 `continuation` 执行：`continue_turn` 表示结果当前已就绪，可立即使用；若是出让控制权的返回值，则等待运行时唤醒。切勿为了等待结果而在轮次内反复轮询 `TaskOutput`。
5. 对照委派范围核查子 Agent 或对等 Agent 提供的证据。在采纳结果前，复现关键检查并审查被修改的文件。
6. 结合未覆盖的验证缺口与产物存储路径，汇总交付综合答复。严禁把确认收到的回执包装为已完成的审计结论。

如需追加跟进输入，先检查任务状态。仅当句柄支持时才调用 `TaskInput`。终态任务无法通过追加输入重新唤醒；如需继续工作，应重新调用该保留的 Agent 或发送授权的持久化消息。

### 子 Agent 生命周期与安全清理

调用完成后，子 Agent 实例会继续保留。只要其上下文仍有价值，就可以持续复用。任务彻底结束后：

1. 保留所需产物，合并或妥善保留其 Worktree 中的改动。
2. 检查父 Agent 的 `GetAgent` 摘要中列出的 `subagent_cleanup` 清理阻塞项。
3. 读取子 Agent `GetAgent(detail="full")` 结果以获取其当前的 `identity.incarnation` 代次。
4. 携带该 Agent ID 与 incarnation 调用 `DeleteAgent`。若报告存在活跃执行、后代实例、脏 Worktree 或共享 Worktree 等阻塞项，需先将其妥善处理。

仅允许清理处于你监督之下的子 Agent。对等 Agent 的回复绝不赋予对其的所有权；切勿仅仅因为对等任务到达终态而删除对等 Agent。

## 使用模式

### 并行调查

分发互不重叠的问题并保留各个句柄。在最终汇报前核实并调和互相冲突的结论。并行度应严格限制在确实能够彼此独立推进的工作范围内。

### 专业化委派

为任务挑选已安装的对应模板，但在委派指令中务必明确验收标准。单纯指定模板角色名称并不能告诉子 Agent 应该交付什么成果。

### 安全试验

对于授权进行的探索性试验，为子 Agent 分配独立 Worktree，并要求其返回 Diff 与测试结果。在合并前先检查该 Worktree；切勿假设改动已经存在于父工作区中。

## 最佳实践

若委派调用被拒绝，核查目标 ID、模板可用性以及调用者权限。切勿通过创建同名替代 Agent 或私自扩大权限来绕过拒绝策略。如果子 Agent 正在等待输入，请针对其具体问题进行答复，或在不再需要其工作时主动终止任务。

成功的委派意味着：父 Agent 拿到了经过充分验证的结论，清楚产物保存位置，并且要么主动保留子 Agent 供后续复用，要么完成了安全的清理闭环。

## 另见

- [多 Agent 协作](../concepts/multi-agent-collaboration.md)
- [任务规格（Tasks specification）](../spec/tasks.md)
- [观察 Agent 工作](observe-agent-work.md)
