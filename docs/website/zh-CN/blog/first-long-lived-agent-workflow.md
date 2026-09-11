---
title: "用 Holon 构建你的第一个长期 Agent 工作流"
summary: "从安装开始，建立具名 Agent、真实工作区、显式 WorkItem 和人工审批边界。"
order: 30
---

# 用 Holon 构建你的第一个长期 Agent 工作流

理解长期 Agent 并不需要先搭建复杂自动化系统。最快的方法，是让一个 Agent 承担一项很小、但天然会跨越决策边界的职责。

在这篇指南中，你将完成：

1. 安装并配置 Holon；
2. 启动本地 daemon；
3. 创建一个具名 Agent；
4. 把一个真实仓库附加为它的工作区；
5. 要求 Agent 创建 WorkItem，并调查一项小改进；
6. 要求它在编辑前等待你的批准；
7. 断开连接，稍后重新进入，批准变更并收到最终 brief。

这个流程可以展示最关键的生命周期，不需要配置 webhook、CI 集成或公网服务器。

**预计时间：** 约 10–15 分钟。  
**文件影响：** 最后一步可能修改你选择的仓库中的一个小文件。如果不希望影响正在进行的工作，请使用临时仓库或干净工作区。

## 前置条件

你需要：

- macOS 或 Linux 终端；
- Holon 支持的模型 Provider 账号；
- 一个可以安全检查、并可选择修改的本地 Git 仓库；
- 使用 Homebrew 完成最短安装路径，或自行下载 Holon Release 二进制。

Holon 仍处于早期阶段。如果下面的命令与当前版本不一致，请以最新 Release Notes 和文档为准。

## 1. 安装 Holon

使用 Homebrew：

```bash
brew tap holon-run/tap
brew install holon
```

确认 CLI 可用：

```bash
holon --help
```

如果不使用 Homebrew，可以从 GitHub Releases 下载当前二进制，或者使用 Cargo 构建仓库。Getting Started 文档列出了当前发布目标。

## 2. 配置模型 Provider

运行交互式 onboarding：

```bash
holon onboard
```

向导会带你完成 Provider 选择、凭据录入、默认模型选择和可选搜索配置。

优先使用 `holon onboard` 提供的凭据流程。不要把 API Key 粘贴到 Prompt、仓库、截图或公开 Issue 中。

完成后检查配置：

```bash
holon config get model.default
holon config doctor
```

如果 `config doctor` 报错，请先修复配置。没有可用模型配置，Agent 无法完成有效工作。

## 3. 启动持续运行时

一次性命令适合快速任务，但持续工作需要 daemon：

```bash
holon daemon start
holon daemon status
```

Daemon 会让 Agent 状态、队列、WorkItem 和等待条件独立于终端界面持续存在。你可以关闭客户端并稍后重连，而不需要让当前聊天窗口成为工作的唯一所有者。

之后如需停止：

```bash
holon daemon stop
```

本教程剩余步骤中请保持它运行。

## 4. 准备一个小仓库

你可以使用现有的干净仓库。如果想要一个临时示例，可以创建：

```bash
mkdir -p ~/tmp/holon-first-workflow
cd ~/tmp/holon-first-workflow
git init
printf '# Demo project\n\nA small repository for testing a durable agent workflow.\n' > README.md
git add README.md
git commit -m 'docs: initialize demo project'
```

如果 Git 要求用户名或邮箱，可以只在本地配置，或者换用已有提交的仓库。

在交给 Agent 前检查工作区：

```bash
git status --short
```

对本教程而言，最安全的起点是命令没有输出，即工作区干净。

## 5. 创建具名 Agent

为这项职责创建一个稳定 Agent：

```bash
holon agent create maintainer
holon agent list
```

Agent 会拥有自己的 Agent Home、角色指令、历史和持续工作状态。`maintainer` 只是示例名称；实际使用时应该选择能够说明长期职责的角色名。

## 6. 把仓库附加为 Agent 工作区

运行：

```bash
holon workspace attach --agent maintainer ~/tmp/holon-first-workflow
```

如果你使用其他仓库，请替换路径。

工作区不只是 shell 中的一次 `cd`。它定义 Agent 读取文件、解析工作区指令、运行命令和应用变更的执行根目录，而且绑定会跨会话保留。

## 7. 通过 TUI 启动一个 WorkItem

打开终端界面：

```bash
holon tui
```

选择 `maintainer` Agent，然后发送：

```text
检查当前仓库，并创建一个 WorkItem，用来改进新贡献者第一次阅读 README 时的体验。

先阅读仓库，提出一项小而可验证的文档改进，记录简短计划和完成标准。暂时不要编辑文件，先请求我的批准并等待。
```

这段文字本身没有特殊语法，它只是把期望生命周期说清楚：

- 目标应该可持续保存；
- Agent 必须检查真实工作区；
- 变更必须小而且可验证；
- 当前还没有编辑授权；
- 下一状态应该是等待操作者输入。

检查 Agent 的回复。它应该说明仓库现状、创建或锚定 WorkItem、提出有边界的改进，并停在审批点。

## 8. 断开连接，但保留责任

使用 `Ctrl+C` 退出 TUI。

客户端已经断开，但 daemon 和 Agent 的持续状态仍然存在。可以从另一个终端检查 Agent：

```bash
holon agent status maintainer
```

然后重新连接：

```bash
holon tui
```

再次选择 `maintainer`。真正要检查的不是每一行聊天是否都在眼前，而是 Agent 能否识别同一目标、说明自己在等什么，并回到对应 WorkItem。

## 9. 批准有边界的变更

如果方案可以接受，回复：

```text
批准。只完成刚才提出的 README 变更，检查最终 diff 和仓库状态，然后用简洁 brief 完成 WorkItem。不要提交 commit。
```

如果方案不合适，不要批准，直接调整约束：

```text
不要执行刚才的方案。修改计划，只增加一个简短的“How to run”章节，不改已有文字，然后再次等待批准。
```

这不是流程失败，而是流程本身的一部分。持续 Agent 应该在操作者改变授权计划时保留工作，而不是丢失目标。

## 10. 检查结果

Agent 完成后，自己检查仓库：

```bash
cd ~/tmp/holon-first-workflow
git diff -- README.md
git status --short
```

一份有用的最终 brief 应该说明：

- 修改了什么；
- 修改了哪个文件；
- 运行了什么验证；
- 是否发生失败；
- WorkItem 是否完成；
- 因为没有得到授权，所以没有创建 commit。

内部执行可能包含多次读取和检查；面向用户的 brief 应该保留决策所需结果，而不要求你阅读所有工具调用。

## 你刚刚验证了什么

这个小工作流覆盖了长期 Agent 的核心组成部分。

### 稳定身份

`maintainer` Agent 独立于一次终端会话持续存在。

### 真实工作区

Agent 在你选择的仓库中工作，使用实际文件和本地工具环境。

### 持续目标

WorkItem 保存职责、计划、进度、等待状态和完成边界。

### 显式人工控制

Agent 可以先检查并提出方案，在获得授权前不编辑。你稍后的输入会让工作从等待变为可继续推进。

### 恢复与交付

关闭 TUI 不会重新定义任务。重连以后，Agent 可以恢复同一项工作并最终交付简洁 brief。

## 从等待操作者扩展到等待外部事件

本教程使用人工批准，是因为它安全而且容易观察。同一套运行时模型也可以表达其他等待：

- 后台构建或测试任务完成；
- CI 状态变化；
- Pull Request 收到 Review；
- 定时器到达复查时间；
- 已批准的外部集成发送事件。

外部唤醒需要主动配置。回调端点、访问 Token 和其他能力 URL 都应该被视为秘密，不要出现在教程、截图、仓库或社交内容中。

## 下一步实验

基础生命周期验证完成后，可以把演示任务替换为一项真实职责：

- 跟踪 Pull Request，直到 CI 和 Review 得到处理；
- 调查 Issue，并等待复现步骤或日志包；
- 准备发布，在公开发布前等待人工批准；
- 跟踪运维问题，在已知外部对象变化时恢复。

保持完成条件可衡量。先从一个 Agent、一项职责开始，再增加角色或集成。

Holon 的核心并不是让 Agent 永远行动，而是让责任在暂时无法行动时仍然存在。

**下一步：** 阅读 [Durable Agent Workflow](/guides/durable-agent-workflow)、
[WorkItems](/guides/work-items)、[Workspaces](/guides/workspaces) 和
[Trust Boundaries](/concepts/trust-boundaries) 指南（英文），在明确边界后扩展这个例子。
