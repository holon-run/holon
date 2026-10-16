---
title: 在项目工作区或 worktree 中工作
summary: 挂载代码项目、激活执行根、在受控 worktree 中隔离改动并安全清理。
order: 60
---

# 在项目工作区或 worktree 中工作

通过工作区激活机制，可以明确指定运行时工具在哪个目录下操作。在终端运行 `cd` 命令仅改变当前命令进程的工作目录，不会持久切换工作区。概念与心智模型请参阅[工作区与工作目录](/zh-CN/concepts/workspaces.md)。

## 前置条件

- 已创建的 Agent 以及本地可访问的项目代码目录。
- 若需要关联独立 worktree，该项目必须是 Git 仓库。
- 具备修改该项目的系统权限；工作区并不是文件系统沙箱。
- 明确知道基于哪个 Base Ref 创建新分支，以进行改动隔离。

以下 Shell 命令在终端中运行；JSON 示例代表 Agent 工具调用，需要由 Agent 通过其暴露的工具接口发出。系统中没有名为 `holon workspace switch` 的 CLI 命令。

## 操作步骤

1. 将项目挂载至目标 Agent：

   ```bash
   holon workspace attach --agent developer /absolute/path/to/my-repo
   holon agent get developer
   ```

   挂载只是让该工作区对 Agent 可用，并不会自动将其设为活动工作区。

2. 要求 Agent 激活该工作区：
   在会话中指示 Agent 切换上下文。Agent 在底层会调用 `SwitchWorkspace` 工具：

   ```json
   {
     "path": "/absolute/path/to/my-repo"
   }
   ```

   激活后，相对路径操作和命令执行的默认执行根目录就会切换为该代码仓库。

3. 创建并激活关联的 Git Worktree 进行改动隔离：
   当需要与主检出目录隔离（例如并行进行代码重构或审查）时，要求 Agent 调用 `CreateWorktree`：

   ```json
   {
     "workspace_id": "ws_12345",
     "branch": "refactor/payment-api",
     "base_ref": "main"
   }
   ```

   创建成功后，系统会自动将活动执行根切换为新创建的 worktree 物理目录。

4. 验证当前执行根：
   检查 Agent 当前的执行环境摘要，确认 `execution_root` 指向预期的 worktree 路径：

   ```bash
   holon agent get developer --detail full | grep execution_root
   ```

5. 完成后安全清理 Worktree：
   当工作合并或完成后，要求 Agent 切换回主工作区并调用 `RemoveWorktree` 清理临时 worktree：

   ```json
   {
     "path": "/path/to/worktree",
     "return_to": "canonical"
   }
   ```

   若 worktree 存在未提交修改或被其他任务占用，安全清理会被系统拒绝。

## 另见

- [工作区与工作目录概念](/zh-CN/concepts/workspaces.md)
- [工作区参考](/zh-CN/reference/workspaces.md)
- [安全与执行边界](/zh-CN/concepts/security-and-execution-boundaries.md)
