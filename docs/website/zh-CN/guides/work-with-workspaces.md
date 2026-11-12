---
title: 在项目工作区与 Git Worktree 中工作
summary: 在对话中直接挂载本地代码仓库、在受控 Git Worktree 中隔离编码改动并完成本地验证，安全清理临时工作树而不污染主工作区。
order: 60
---

# 在项目工作区与 Git Worktree 中工作

当让 AI 编程 Agent 检查或修改本地代码时，如果直接让它在你的主工作区中操作，存在不小的风险：未暂存的临时调试配置、编写到一半的代码或当前检出的分支，可能被 Agent 的误操作污染甚至冲掉。

Holon 将工作区（Workspace）与 Git Worktree 作为一等公民（First-Class Citizens）进行管理。无需全量重新 clone 庞大的仓库，也无需冒着工作区被弄脏的风险，你只需在对话中直接吩咐 Agent 挂载项目，并在几秒内基于指定分支建立独立的 Git Worktree。Agent 会在拥有独立 HEAD 和文件树的分支中开展工作，同时零冗余复用底层相同的 `.git` 对象库。

本指南演示如何在 Web GUI 对话中用自然语言让 Agent 挂载项目、建立隔离 Worktree、执行改动与测试验证并安全收尾清理，全程无需记忆底层 CLI 命令或手动拼装 JSON 工具参数。关于底层架构与心智模型，请参阅[工作区与工作目录概念](../concepts/workspaces.md)。

## 前置条件

- 本地已启动 Holon 并可访问 [Web GUI](./use-web-gui.md)（默认 `http://localhost:8080`），且已[配置可用模型](./configure-models.md)。
- 已有至少一个 Agent（如 `developer` 或 `main`）。
- 本地机器上存在一个由 Git 管理的代码仓库绝对路径，且你拥有修改权限（例如 `/home/user/src/github.com/my-org/my-app`）。
- 无需手动在终端运行底层命令或手动拼装工具调用：Agent 可以直接在对话中自主执行挂载、建树与安全清理。

## 操作步骤

### 1. 在对话中挂载并激活项目工作区

在 Web GUI 聊天窗口中，向你的 Agent 发送提示词，要求其挂载本地代码仓库并设为当前活动工作区：

> “请将本地仓库 `/home/user/src/github.com/my-org/my-app` 挂载为工作区并激活，后续操作在这个项目中进行。”

**背后运行机制：**
- Agent 在底层调用 `AttachWorkspace` 将代码目录绑定到当前 Agent，随后调用 `SwitchWorkspace` 将该目录激活为当前执行根（Execution Root）。
- 此后，所有相对路径操作（如代码补丁 `ApplyPatch`）和命令执行默认均以此仓库根目录为基准，而不是 Agent 自身的个人目录（Agent Home）。
- 注意：在终端命令中单次执行 `cd` 只会影响该子进程自身，无法持久切换 Agent 的活动执行根；必须通过工作区激活机制切换。

### 2. 要求 Agent 在独立 Git Worktree 中隔离任务

在让 Agent 开始修改代码或重构前，指示其基于指定的基准分支创建独立 Worktree：

> “请基于 `main` 分支创建一个名为 `feat/user-auth` 的独立 worktree，本次代码改动和测试全部在里面进行，不要碰主分支与既有工作区。”

**背后运行机制：**
- Agent 调用内置的 `CreateWorktree` 工具。Holon 会在受控目录中秒级建立关联的 Git Worktree，并将活动的执行根自动切换至该新目录。
- Worktree 与本地主仓库共享同一个 `.git` 数据对象库，秒级创建且几乎不占用额外磁盘空间。
- Agent 所在的分支 `feat/user-auth` 拥有独立的文件树与 HEAD 指针。即使你的主检出目录中包含未提交的代码或正处于其他分支，也完全不受干扰。

### 3. 在 Worktree 中开展编码与本地验证

接下来指示 Agent 执行具体的开发任务并运行项目测试：

> “请在 `src/auth.rs` 中实现用户登录接口，补全单元测试，并运行 `cargo test --lib auth` 确保全部通过。”

**背后运行机制：**
- Agent 的所有代码修改均限制在该 Worktree 目录下。
- 测试命令、代码检查器（Linter）与编译均严格在该 Worktree 的执行根中运行。
- 测试通过后，Agent 会将改动提交到专用的 `feat/user-auth` 分支，或生成清晰的 Git Commit / 补丁供你发起 Pull Request。

### 4. 任务完成后安全清理 Worktree

当改动已提交、推送或提交为 PR 后，吩咐 Agent 切回项目根目录并清理临时 Worktree：

> “PR 已提交。请切回主仓库工作区，并安全清理刚才的 `feat/user-auth` worktree，保留该分支。”

**背后安全防护机制：**
- Agent 首先切回主仓库工作区，随后调用 `RemoveWorktree`（携带 `branch_policy: "keep"`）安全移除该 Worktree 的物理目录，同时保留 Git 分支与提交历史。
- **运行时的三重安全防御：**
  1. **未提交改动（Dirty）防御**：若 Worktree 中存在未暂存或未提交的文件，运行时会坚决拒绝删除，防止代码丢失；
  2. **活跃占用保护**：若仍有正在执行的后台命令或子任务占用该目录，清理会被安全拦截；
  3. **受控路径验证**：仅允许清理在 Holon 中注册的托管 Worktree，拒绝任意未经注册的系统路径。

## 关键机制解析

### 执行根（Execution Root） vs 单次 Shell `cd`

很多开发者容易混淆“子进程目录切换”与“运行时执行根”：

- **Shell `cd`**：仅在单个子进程内部临时切换目录。该命令结束后，后续的命令执行或文件补丁工具依然会回到 Agent 的活动执行根中。
- **工作区激活（`SwitchWorkspace`）**：持久性改变运行时的执行根，重置相对路径补丁（`ApplyPatch`）的基准，并重新加载该仓库树下的 `AGENTS.md` 规则。

### 为什么 Git Worktree 优于重新 Clone

| 方案 | 创建速度 | 磁盘开销 | 本地未推送上下文 |
|---|---|---|---|
| 直接在主检出目录修改 | 即时 | 无额外开销 | 高风险：容易污染甚至覆盖未提交的本地代码 |
| 完整 `git clone` | 缓慢（大仓库需要数分钟） | 巨大（重复下载全部 Git 历史） | 丢失本地未推送的分支与 stash 上下文 |
| **Git Worktree 隔离** | **秒级（毫秒级建立）** | **极小（复用本地 `.git` 对象库）** | **完整读取本地所有分支、Tag 与提交** |

## 可选：面向自动化脚本的 CLI 命令

虽然在 Web GUI 中使用自然语言是推荐的日常交互方式，但 Holon 也为自动化流水线或无头脚本提供了等效的 CLI 命令：

```bash
# 1. 为指定 Agent 挂载本地代码仓库
holon workspace attach --agent developer /home/user/src/github.com/my-org/my-app

# 2. 查看 Agent 当前绑定的工作区与执行根详情
holon agent get developer --detail full

# 3. 当不再需要时解绑工作区
holon workspace detach --agent developer ws_12345

# 4. 退出当前工作区回到 Agent Home
holon workspace exit --agent developer
```

## 相关文档

- [工作区与工作目录概念](../concepts/workspaces.md)：深入理解 Agent Home、工作区与执行根的设计逻辑。
- [搭建文档管家 Agent](./build-docs-steward.md)：实战案例，观察 Agent 如何在独立 Worktree 中审计文档漂移。
- [搭建代码审查 Agent](./build-code-reviewer.md)：学习如何利用隔离 Worktree 安全运行不可信分支审查。
- [工作区参考](../reference/workspaces.md)：查看完整的参数 Schema、错误码与底层接口。
