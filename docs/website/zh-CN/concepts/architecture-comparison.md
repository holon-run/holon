---
title: 架构对比
summary: 对比 Holon 自有 Rust Agent 运行时与基于外部 CLI 编排的自托管 AI 助手、嵌入式 Harness 及闭源云端 Bot 平台。
order: 160
---

# 架构对比：自托管 AI 助手 vs. Agent 运行时

如果你正在评估**自托管 AI 助手**（包括 Grok Bot 或 OpenClaw 的开源替代方案），或者对比各类 **Agent 运行时**，大多数工具实际上处于三个不同的架构层级。仅从界面特征（例如是否内置语音或浏览器面板）来对比，往往会掩盖最核心的问题：**任务执行、等待挂起、工作区隔离以及信任边界究竟由谁掌控？**

## 三个架构层级

| 类别 | 代表项目 | Agent 主循环归属 | 核心优势 | 权衡取舍 |
|---|---|---|---|---|
| **自有 Agent 运行时** | **Holon**（Rust 守护进程） | Holon 自身（`holon`） | 持久化 `WorkItem`、事件驱动的等待/唤醒、关联 `git worktree` 隔离、显式来源与信任边界、模型无关路由 | 当前生态连接器覆盖面较窄（尚无内置 MCP 客户端、实时语音或内置浏览器） |
| **外部编码 CLI 编排壳** | OpenMausBot、Codync、`nightly-labs/OpenBot` | 外部 CLI（`Claude Code`、`Codex`、`Cursor`、`OpenCode` 等） | 直接复用既有 CLI 订阅、Skill 与 MCP 服务器；具备丰富的桌面/移动/语音交互与共享聊天界面 | 执行生命周期、故障恢复与权限边界受制于被封装的底层 CLI |
| **嵌入式运行时应用与闭源云端 Bot** | Rakazo（嵌入 Pi）、Grok Bot（xAI 闭源云端） | 嵌入式 JS 运行时或厂商云端宿主 | 面向消费级用户的快速上手；云端工作区、内置语音或移动端单动作 UI | 单一厂商锁定（云端方案），或耦合特定 Docker / Harness 而非独立 Rust 守护进程 |

## Holon 在运行时行为上的差异

表层功能对比表通常会问：一个工具是否有桌面客户端、移动 App 或定时任务。但在自己的电脑或服务器上运行长周期任务时，更棘手的故障往往发生在运行时这一层：

1. **客户端断开与进程重启后工作依然存活。** 各类客户端（Web GUI、TUI、macOS 菜单栏应用、iOS/Android 远程客户端）共同连接一个后台 `holon` 守护进程。任务句柄（`TaskHandle`）与持久化目标（`WorkItem`）保存在运行时管理的 SQLite 状态中，而不是绑定在某个浏览器标签页或终端复用器里。
2. **显式的等待与唤醒语义。** Agent 通过 `WaitFor` 条件（`task_result`、`timer`、`external`、`operator_input`）干净地挂起，而不是在循环里轮询空转。外部 Webhook 或定时触发器仅作为唤醒提示（wake hint）驱动重新核验——外部触发器不会自动授予操作者权威。
3. **来源与信任边界分离。** 运行时明确区分操作者指令、Agent 自有指引、工具结果、子 Agent 汇报以及外部 Webhook/App 载荷。任务执行期间读取的不可信外部文本，无法自我升级为操作者指令。
4. **工作区与并行分支隔离。** 每个 Agent 将持久化的 `agent_home`（身份、记忆、笔记、私有状态）与挂载的项目工作区明确分离。针对同一代码库的并行工作使用已登记的关联 `git worktree` 根目录（`CreateWorktree` / `RemoveWorktree`）并附带活跃占用检查，防止并发任务互相覆盖。
5. **模型无关的本地与云端路由。** Holon 可对接 OpenAI 兼容端点、Anthropic、Gemini 以及 Ollama、`llama.cpp` 等本地推理服务，无需绑定特定厂商的编码 CLI 订阅。

## 当前能力覆盖与已交付边界

将 Holon 与 Grok Bot 或基于 CLI 编排的自托管助手对比时，应同时评估当前已交付能力与明确尚未内置的范围：

| 维度 | Holon 当前实现（仓库实证） | 评估说明 |
|---|---|---|
| **运行时与部署** | Rust 二进制（`holon`）、本地守护进程、Docker 镜像 | 单机守护进程；支持 Linux amd64、macOS arm64/amd64 |
| **客户端** | Web GUI、TUI、CLI、macOS 菜单栏应用（`HolonMenu`）、iOS 与 Android 远程应用 | 手机端是连接你自有守护进程的远程客户端，而非端侧离线本地 Agent |
| **多 Agent 与模板** | 25+ 检入仓库的 `agent_templates`、`InvokeAgent` 受监督子 Agent、对等 `SendAgentMessage` | 按角色隔离的 `skills/`、独立 `agent_home`、显式清理规则 |
| **定时与外部事件** | 持久化定时器（`interval_ms` / `calendar_expression`）、Webhook 接入、`AgentInbox` | 唤醒 Agent 绝不绕过信任分级或事实核验 |
| **宿主机执行** | 工作区作用域内的 Shell 命令、补丁与后台 `command_task` | 宿主本地执行（`host_local`），不包含桌面鼠标/屏幕 GUI 自动化或单次动作级 VM |
| **MCP 支持** | **尚未内置** | 外部 CLI 工具通过 `skills/` 封装接入；尚无原生 MCP 客户端或服务端 |
| **实时语音（TTS/STT）** | **尚未内置** | 仅支持文本、图像、音视频文件与产物工作流 |
| **内置浏览器** | **尚未内置** | 运行时内部不捆绑无头浏览器引擎；外部浏览器 Skill 为可选项 |
| **桌面客户端覆盖** | macOS 菜单栏应用 + 跨平台 Web GUI 与 TUI | 暂未提供原生 Windows 或 Linux Electron 桌面封装 |

## 如何选择适合的方案

- **选择 Holon**：如果你希望在自己的电脑或服务器上运行一套自托管、模型无关的 AI 助手工作台，让 Agent 能够跨轮次跟踪 `WorkItem`、在不空转轮询的情况下等待 CI/定时器/Webhook、用 Git worktree 隔离并行代码修改，并坚持显式的信任边界。
- **选择 CLI 编排壳**（如 OpenMausBot、Codync 或 `nightly-labs/OpenBot`）：如果你的首要目标是在现有的 Claude Code、Codex 或 Cursor CLI 会话之上套一层聊天/语音/移动端界面，并直接复用它们已有的 MCP 生态。
- **选择云端托管 Bot**（如 Grok Bot）：如果你更看重零本地配置与厂商托管执行，而非自主托管、本地仓库直连以及模型自由选择。

## 如何在本地自行验证这些能力

你不需要额外的合成基准测试套件，按照常规用户指南即可在本地亲自核验 Holon 的运行时行为，并用只读命令检查状态：

1. **重启存活与零 Token 空等：** 按[运行长生命周期任务](../guides/run-long-lived-task.md)操作。当任务处于等待态时，检查当前工作项与定时器，随后重启守护进程：
   ```bash
   holon work-item list --agent main
   holon timer list --agent main
   holon daemon stop && holon daemon start
   holon daemon status
   ```
   重启前后同一个 `WorkItem` 与定时器状态完整保留，并在到达触发时间时恢复执行，期间不产生轮询空转。
2. **物理 Git worktree 隔离：** 按[在项目工作区或 Git worktree 中工作](../guides/work-with-workspaces.md)操作，并在仓库目录下运行 `git worktree list`。Agent 的改动留在独立的关联 worktree 分支中，主工作目录保持干净。
3. **外部唤醒与信任分离：** 按[接入外部事件](../guides/connect-external-events.md)操作。外部 Webhook 回调仅作为不可信的 `wake_hint` 唤醒 Agent，绝不会授予操作者权威或绕过事实核查。

*适用范围说明：* Holon 目前是单机守护进程（非多活高可用集群），挂起等待意味着 Agent 处于休眠而非自旋，外部唤醒仅证明触发信号已送达——不代表第三方载荷里的主张天然真实。

## 延伸阅读

- [运行时模型](./runtime-model.md) —— 守护进程、客户端、Agent、工作项与工作区如何协同运作
- [工作项与任务](./work-items-and-tasks.md) —— 持久化目标与受监督执行句柄
- [事件、等待与唤醒](./events-and-waits.md) —— Agent 如何在定时器、任务与外部唤醒之间暂停并恢复工作
- [信任边界](./trust-boundaries.md) 与 [安全与执行边界](./security-and-execution-boundaries.md) —— 权威隔离与宿主本地执行限制
- [运行长生命周期任务](../guides/run-long-lived-task.md)、[在项目工作区或 Git worktree 中工作](../guides/work-with-workspaces.md) 与 [接入外部事件](../guides/connect-external-events.md) —— 实操指南
- [开始使用](../getting-started/) —— 安装 Holon 并跑通第一个本地助手
