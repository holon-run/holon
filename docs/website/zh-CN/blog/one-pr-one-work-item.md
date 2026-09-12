---
title: "不止审一次代码：用 Holon 搭建持续跟进 PR 的 Reviewer"
summary: "从模板创建自带工作规范与 Skills 的 reviewer，确认职责与合并权限，订阅仓库 PR，自动审阅并持续跟进修复与 CI。"
order: 30
---

# 不止审一次代码：用 Holon 搭建持续跟进 PR 的 Reviewer

![蓝色 Reviewer 面对代码面板，前方的 PR 流程依次展示修改、等待与检查通过。](/assets/continuous-pr-reviewer-cover.webp)

> **中文审阅稿 · 未发布。** 案例来自 Holon 仓库自身的 `holon-reviewer`；案例记录采集于 2026 年 9 月 10 日。

作者推送了修复，reviewer 确认原来的问题已经解决。几分钟后，CI 又红了。

Holon 仓库的 PR #2854 就经历过这个过程。它修改了工作项的完成行为：Agent 完成另一项工作时，不应连带结束自己正在执行的工作。第一次修复解决了调度问题，却带入了错误的测试期望；reviewer 再次提出阻塞意见，直到作者修正、最终版本的检查通过，才进入合并与收尾。

| 代码版本 | 审阅判断 | 后续动作 |
| --- | --- | --- |
| `589405cc` | 两个调度端到端测试失败，定位到工作项分类错误 | 提出阻塞意见，等待修复 |
| `19f390ca` | 原问题已修复，调度检查通过；另一条测试期望被改反 | 再次提出阻塞意见，等待新版本 |
| `f7b93809` | 测试期望恢复，最终版本的相关检查通过 | 确认合并结果，完成工作项 |

这一过程发生在 2026 年 9 月 9 日，提交、公开审阅与合并结果已和 GitHub 快照核对。部分 CI 任务按条件跳过，表中的“通过”只指实际执行的相关检查。

下面创建一个 reviewer，让它自动接手仓库的新 PR，在修复和 CI 更新后继续跟进。

## 从模板创建 Reviewer

先按[创建第一个 Agent](../getting-started/first-agent.md)完成 Holon 安装、模型配置，并启动常驻 runtime。运行 Holon 的机器需要能访问目标仓库，备好 GitHub CLI（`gh`）及项目测试工具。持续接收事件还会用到 AgentInbox 和 UXC，后面会检查接入。

在自己的终端中，从官方模板创建一个名为 `reviewer` 的 Agent：

```bash
holon agent create reviewer --template https://github.com/holon-run/holon/tree/main/agent_templates/code-reviewer
```

如果已经安装或同步了该模板，也可以用模板名；两种方式选一种即可：

```bash
holon agent create reviewer --template code-reviewer
```

`reviewer` 是你创建的 Agent 名称，`code-reviewer` 是模板名。若已有同名 Agent，换一个名称，不要覆盖原来的工作记录。

创建后运行 `holon agent list`，确认新 Agent 出现在列表中，再从 TUI 或 Web GUI 选择它。

### 模板内置的 AGENTS.md

`code-reviewer` 模板包含 `AGENTS.md`，创建时用它初始化新 Agent 的工作规范，并安装模板声明的 Skills。下面摘录文件中的职责定义和权限确认清单：

```markdown
# Holon Reviewer Agent

You are a long-lived code review agent responsible for code review, PR
lifecycle tracking, and merge decisions.

## Permission Confirmation Protocol

For **non-one-time** review work, confirm the following with the operator
before starting, then record the confirmed scope in your agent-local
AGENTS.md:

- whether you may merge PRs
- whether you should subscribe to PR events via `agentinbox` follow
- whether you may approve PRs
- whether you may fix code on behalf of the author
```

这份规范要求 Agent 审代码、跟进 PR、判断能否合并。开始持续工作前，它需要向你确认权限，并记入自己的 `AGENTS.md`。安装模板不会自动授予合并权限。

文件后面的章节规定了怎么工作：

- **持续跟进**：为 PR 建立 WorkItem，订阅新提交、CI 和审阅评论；新版本到达后先复查旧问题，合并或关闭后完成工作项、清理订阅。
- **合并门槛**：最终 head 的必要 CI 全部通过，没有未解决的阻塞问题；普通建议不应被当成阻塞，也不能绕过 GitHub 的平台限制。
- **升级处理**：大规模重构、破坏性 API 变更或安全敏感修改交给操作者判断；未经授权，不主动代作者修代码。

审阅的具体方法由 Skills 提供：`code-review` 规定证据、问题分类和验证范围，`github-review` 负责 GitHub 上的上下文收集、去重与审阅发布。`ghx`、`sview` 辅助平台操作和源码阅读，`agentinbox`、`uxc` 负责事件接入。

你只需补充项目要求，不用从头写审阅提示词。要调整通用职责或替换 Skills，可以参考[Agent 模板指南](../../guides/agent-templates.md)。

## 开始前确认职责与合并权限

向 reviewer 说明负责的仓库、项目要求和操作权限：

```text
负责 <owner/repo> 的 PR，订阅仓库并自动审阅新 PR，持续跟进到合并或关闭。
本地仓库在 <绝对路径>，重点看兼容性，中文反馈；允许隔离测试、评论和批准。
最终版本的必要检查通过、无遗留阻塞且符合仓库合并规则时，可以直接合并。
不代作者修代码；重大或安全敏感变更先问我。请记住这些长期要求。
```

让 reviewer 确认并记入自己的 `AGENTS.md`，后续新 PR 沿用这份授权。构建、测试和代码约定优先读取仓库本身的 `AGENTS.md`；如果团队只允许 squash 合并，也在这里说明。

这份授权还需要 GitHub 账号具备相应权限，并遵守仓库保护规则。模板不提供凭据。首次试用选自己熟悉的低风险 PR，并为测试准备隔离环境。

## 订阅仓库，自动发现 PR

职责确认后，接通事件来源。Holon 仓库自身的 `holon-reviewer` 在工作规范中采用两层订阅：

- **仓库订阅长期保留**，发现新开的 PR，让 reviewer 自动开始审阅。
- **每条 PR 单独跟进**，接收后续提交、CI 和审阅评论；合并或关闭后，清理该 PR 的订阅。

模板提供逐 PR 跟进规则与接入 Skills，前面的授权确定仓库和自动接手范围。外部服务仍需单独认证、订阅。

事件通过 AgentInbox 接收，再通知 Holon 恢复工作；GitHub 适配会用到 UXC。让 reviewer 按自带的 Skill 完成接入即可：

```text
请按 agentinbox Skill 接通这个仓库的新 PR 订阅和逐 PR 的 CI、评论跟进，缺少工具或认证时告诉我。
```

reviewer 会按 Skill 检查服务、仓库访问和自己的唤醒目标，并指出需要你完成的安装或认证。凭据通过工具的认证流程配置，不要贴进对话。接入细节见[AgentInbox 接入指南](https://agentinbox.holon.run/guides/onboarding-with-agent-skill)。

接入后，让它确认仓库发现订阅已生效。不要假定新事件订阅会补齐历史；要接管已有的未关闭 PR，再补一句“也接管当前打开的 PR”。

## 检查它是否自动开始审阅

等仓库出现一条适合试运行的新 PR，观察 reviewer 能否从事件发现它，自动建立 WorkItem 并开始审阅。这次不要手动发送 PR 地址。

如果新 PR 出现后没有开始审阅，先检查仓库发现订阅、AgentInbox 的事件记录和 reviewer 的唤醒目标。手动发一条 PR 可以测试单次审阅，但不能证明仓库自动接入已经生效。

打开这条 PR 对应的 WorkItem，应该能找到已审 head、问题与证据、已经完成的验证，以及下一步在等什么。修复到达时，reviewer 就能拿新版本对照上次的阻塞项。

首次审阅结果可以按这份示例检查：

```text
已审版本：<head SHA>
阻塞问题：<问题、位置与依据>；或本次未发现阻塞问题
验证：<已执行的测试和结果>；<未覆盖的范围>
下一步：等待 <作者修复 / 当前 head 的 CI / 维护者决定>
```

## 验证一次真正的持续复查

先让现有 CI 跑完，检查 reviewer 是否收到事件并更新检查结果。再等作者正常提交更新，观察它是否回到同一条 PR 的 WorkItem，复查新版本。期间不用发“请继续”。CI 通知和新提交都要检查，前者不能替代后者。

核对复查记录：

- 记录写明新 head，引用的 CI 属于这个版本。
- 旧问题逐项说明已修复或仍存在，新增问题另列。
- 写清还在等什么，或需要你做什么决定。

如果 AgentInbox 已收到事件，reviewer 却没有恢复，先检查唤醒目标、runtime 是否在线及工作项等待条件；如果恢复了却仍引用旧 CI，就要求它重新核对提交。补发一句“继续”可以临时推进工作，但不能代替这次接入验证。

PR #2854 的第二版修好了旧问题，却带入了另一个错误。复查记录需要区分两者，让作者知道下一步该改哪里。

## 合并与收尾

最终 head 的必要检查通过、没有遗留阻塞且符合仓库规则后，reviewer 可以按授权直接合并。需要升级处理的变更，或权限、检查不满足要求的 PR，应先说明原因，交给你决定。

交付应写明实际合并结果、最终提交和未验证范围。PR 合并或关闭后，reviewer 按模板要求确认终态、完成 WorkItem，并清理该任务的订阅；共享事件源留给其他工作。

保留仓库订阅，reviewer 就能继续接手新 PR，只在需要你决策时请你介入。若要保留人工合并，将长期授权改为“达到合并条件时通知我，由我合并”即可。

## 案例来源

文中的 PR #2854 使用了完整工作计划及 GitHub 提交、审阅、检查和合并快照。本次未重新执行案例中的测试，也未独立验证订阅清理结果。
