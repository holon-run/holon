---
title: 搭建持续跟进 PR 的本地 Code Reviewer
summary: 从 code-reviewer 模板创建审阅 Agent，直接通过对话让它挂载代码库、记录合并权限并订阅 GitHub 事件，持续跟进新提交与 CI 直至合并。
order: 88
---

# 搭建持续跟进 PR 的本地 Code Reviewer

本指南演示如何基于 `code-reviewer` 模板搭建一个常驻的 GitHub Pull Request 审阅 Agent：你只需在 Web GUI 中创建 Agent，随后通过日常对话让它自己挂载代码仓库、记录审阅与合并边界、接通事件订阅，并在每次新提交或 CI 更新时自动复查。

- 如果你只需要对单个 PR 执行一次命令行审阅，请使用[使用 `holon solve` 运行 GitHub 任务](./run-github-task.md)。
- 如果你想先看这种模式在真实仓库中的运行效果（例如 PR #2854 的多轮复查过程与三个月 95% 的 PR 覆盖率），请参阅博客[《不止审一次代码：用 Holon 搭建持续跟进 PR 的 Reviewer》](../blog/one-pr-one-work-item.md)。

## 开始之前

- 本地已启动 Holon 并可访问 [Web GUI](./use-web-gui.md)（默认 `http://localhost:8080`），且已[配置可用模型](./configure-models.md)。
- 运行 Holon 的机器已安装并登录 GitHub CLI（`gh`），以及目标项目所需的编译与测试工具。
- 无需提前背记 CLI 参数或手写配置文件：克隆仓库、关联工作区、写入权限规则和配置事件订阅，都可以直接在对话里交给 Agent 完成。

## 操作步骤

### 1. 从 `code-reviewer` 模板创建 Agent

`code-reviewer` 模板预置了审阅职责契约（`AGENTS.md`）以及所需的全部技能：
- `code-review` 与 `github-review`：按证据分级问题、过滤已解决评论、避免在同一轮同时发送顶层评论与 Review 造成重复通知；
- `ghx` 与 `sview`：读取 GitHub PR 上下文与本地源码结构；
- `agentinbox` 与 `uxc`：接入仓库 PR 与 CI 事件订阅。

#### 推荐方式：在 Web GUI 中创建

1. 打开 Holon Web GUI，在左侧导航点击 **Templates**（`/templates`）。
2. 如果列表中还没有 `code-reviewer`，点击 **Install Template** 并填入：
   ```text
   https://github.com/holon-run/holon/tree/main/agent_templates/code-reviewer
   ```
3. 在模板列表中找到 `code-reviewer`，点击 **Create Agent**：
   - **Agent ID**：`reviewer`（若已被占用，可填 `repo-reviewer` 等未使用的名称）
   - **名称**：`代码审阅员`
4. 点击确认创建，进入该 Agent 的对话页面。

> **可选（命令行方式）**：如果你习惯终端，也可以运行 `holon agent create reviewer --template code-reviewer` 创建。详见[从模板创建 Agent](./create-agent.md)。

### 2. 在对话中让 Agent 关联代码仓库并记录权限边界

`code-reviewer` 模板内置了权限确认协议（`Permission Confirmation Protocol`）：在开始持续审阅前，Agent 需要明确知道自己能否批准 PR、能否直接合并、是否订阅事件，以及是否允许代作者改代码。

你不需要手动去编辑它的 `AGENTS.md`，也不需要先用命令行挂载目录。直接在对话框中把仓库地址（或本地路径）和团队规则发给它：

```text
请负责 <owner/repo> 的 PR 审阅：
1. 如果本地还没有这个仓库，请把它克隆到本地并添加为你的工作区（如果已在本地 <绝对路径>，直接挂载该路径）。
2. 审阅时结合本地源码和测试验证，不要只猜 diff；优先读取仓库自带的 AGENTS.md 了解构建与测试要求，用中文反馈。
3. 允许在隔离工作树中运行测试、发表审阅评论和 Approve。
4. 当最终提交的必要 CI 全部通过、没有遗留阻塞问题且符合仓库合并规则时，允许使用 squash 直接合并（如果你希望保留人工合并，可改为“达到合并条件时通知我，由我合并”）。
5. 不要直接替作者修改分支代码；遇到大规模重构、破坏性 API 变更或安全敏感修改先问我。
请把以上长期要求写入你的 AGENTS.md 并向我确认。
```

**Agent 会自动完成的事**：
- 若提供的是远程地址，自动克隆仓库并调用内置工作区工具完成挂载；若提供的是本地路径，直接将其绑定为关联工作区；
- 读取目标仓库的构建与测试规范；
- 将你确认的合并门槛、评论语言和升级边界持久化写入它自己的 `~/.holon/agents/reviewer/AGENTS.md`，后续所有 PR 均沿用这份规则。

### 3. 先发一条已有 PR 试跑单次审阅

在开启全自动仓库监听前，建议先拿一条熟悉的 PR 让它试跑一次，确认本地测试环境与审阅口径符合预期。直接在对话框中发送：

```text
先审阅一下 https://github.com/owner/repo/pull/123 ，按刚确认的规则在本地核对相关代码和测试，只发布一次结构化审阅结论，不要重复刷屏。
```

**Agent 会自动完成的事**：
1. 为 `PR #123` 创建独立的 WorkItem，记录当前 `head SHA`；
2. 读取 PR 变更与已有评论，切换到独立 worktree 检出代码，运行受影响的编译与测试；
3. 将问题按阻塞项（Blocking）与非阻塞建议分级，附带文件行号与验证证据发布一次 GitHub Review；
4. 在对话简报与 WorkItem 中给出结构化结论：

```text
已审版本：<head SHA>
阻塞问题：<问题、位置与依据>；或本次未发现阻塞问题
验证：<已执行的测试和结果>；<未覆盖的范围>
下一步：等待 <作者修复 / 当前 head 的 CI / 维护者决定>
```

### 4. 让 Agent 配置仓库与 PR 事件自动跟进

单次审阅验证通过后，就可以让它接通事件源，实现“新 PR 自动接手 + 同一条 PR 持续跟进修订与 CI”。

同样无需自己拼写订阅命令，直接在对话里告诉它：

```text
请按你自带的 agentinbox Skill，接通 <owner/repo> 的新 PR 自动发现订阅，以及每条在审 PR 的新提交、CI 和评论跟进。如果缺少工具或需要我完成认证，请直接告诉我怎么操作。
```

**Agent 会自动完成的事**：
- 按照自带的 `agentinbox` 与 `uxc` 技能，检查本机服务、仓库访问权限和自己的唤醒回调目标；
- 如果本机尚未安装或登录相关工具，它会一步步提示你完成认证（切勿把 Token 直接贴进对话框）；接入细节也可参考 [AgentInbox 接入指南](https://agentinbox.holon.run/guides/onboarding-with-agent-skill)；
- 自动建立两层订阅机制：
  - **仓库级长期订阅**：发现新打开的 PR，自动为其创建 WorkItem 并开始首次审阅；
  - **单条 PR 动态订阅**：在审阅未结束前，监听该 PR 的新 push、CI 状态变化与审阅回复；当 PR 合并或关闭后，自动完成对应的 WorkItem 并清理该 PR 的订阅。

> **提示**：新建立的事件订阅默认只接收后续发生的新事件。如果你希望它同时接手仓库里当前已经打开的 PR，只需在对话中补一句：“请把当前已经打开的 PR 也接管起来。”

### 5. 验证自动复查与合并收尾

配置完成后，观察一次完整的 PR 演进周期（期间无需向它发送“请继续”）：

1. **自动发现新 PR**：当仓库出现新 PR 时，在 Web GUI 的 WorkItems 视图中可以看到 `reviewer` 自动创建了对应的工作项并完成首轮审阅。
2. **推送修复或 CI 完成时自动复查**：当作者推送新 commit 或 CI 跑完时，事件会唤醒同一个 WorkItem。`reviewer` 会先核对最新 `head SHA`，逐条检查上一轮的阻塞问题是否已解决、当前版本的 CI 是否通过，以及是否引入了新问题——**已经解决的旧评论不会再次重复发送**。
3. **合并或升级交接**：
   - 当最新 `head` 的必要检查全部通过且无遗留阻塞时，`reviewer` 按授权执行合并（或通知你合并），随后标记 WorkItem 完成并退订该 PR 的事件；
   - 当遇到需要人工拍板的架构或安全变更时，它会在简报中列出证据与阻塞原因，等待你的指示。

## 常见问题排查

遇到异常时，优先在对话中直接让 `reviewer` 自检：

- **新 PR 出现后没有自动开始审阅**：
  在对话中发送：“请检查你对 `<owner/repo>` 的 AgentInbox 仓库订阅状态和唤醒目标是否正常。” 它会自动核对订阅列表、最近事件记录与回调配置。
- **作者推了新代码，但审阅仍引用旧的 CI 结果**：
  在对话中发送：“请重新核对这条 PR 的最新 head SHA 以及对应版本的 CI 状态。”
- **想调整审阅松紧度或收回自动合并权限**：
  直接在对话中告诉它：“从现在起不要自动合并 PR，达到合并条件时只 Approve 并在对话里通知我；请更新你的 AGENTS.md。”

## 相关内容

- [《不止审一次代码：用 Holon 搭建持续跟进 PR 的 Reviewer》](../blog/one-pr-one-work-item.md)：了解 Holon 自身仓库在 3 个月内用这套方式审阅 763 个已合并 PR 的真实案例与 WorkItem 设计背景。
- [使用 `holon solve` 运行 GitHub 任务](./run-github-task.md)：针对单个 Issue 或 PR 执行一次性命令行任务。
- [连接外部事件与 Webhook](./connect-external-events.md)：了解事件接入、唤醒回调与权限边界。
- [在项目工作区或 worktree 中工作](./work-with-workspaces.md)：了解多工作区挂载与隔离验证机制。
- [从模板创建 Agent](./create-agent.md)：了解模板安装与初始化流程。
