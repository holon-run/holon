---
title: "不止审一次代码：用 Holon 搭建持续跟进 PR 的 Reviewer"
summary: "从模板创建自带工作规范与 Skills 的 reviewer，确认职责与合并权限，订阅仓库 PR，自动审阅并持续跟进修复与 CI。"
order: 30
date: 2026-09-13
---

# 不止审一次代码：用 Holon 搭建持续跟进 PR 的 Reviewer

![蓝色 Reviewer 面对代码面板，前方的 PR 流程依次展示修改、等待与检查通过。](/assets/continuous-pr-reviewer-cover.webp)

> 案例来自 Holon 仓库自身的 `holon-reviewer`；案例记录采集于 2026 年 9 月 10 日。

作者推送了修复，reviewer 确认原来的问题已经解决。几分钟后，CI 又红了。

Holon 仓库的 PR #2854 就经历过这个过程。它修改了工作项的完成行为：Agent 完成另一项工作时，不应连带结束自己正在执行的工作。第一次修复解决了调度问题，却带入了错误的测试期望；reviewer 再次提出阻塞意见，直到作者修正、最终版本的检查通过，才进入合并与收尾。

| 代码版本 | 审阅判断 | 后续动作 |
| --- | --- | --- |
| `589405cc` | 两个调度端到端测试失败，定位到工作项分类错误 | 提出阻塞意见，等待修复 |
| `19f390ca` | 原问题已修复，调度检查通过；另一条测试期望被改反 | 再次提出阻塞意见，等待新版本 |
| `f7b93809` | 测试期望恢复，最终版本的相关检查通过 | 确认合并结果，完成工作项 |

这一过程发生在 2026 年 9 月 9 日，提交、公开审阅与合并结果已和 GitHub 快照核对。部分 CI 任务按条件跳过，表中的“通过”只指实际执行的相关检查。

在 2026 年 6 月 12 日至 9 月 12 日的三个月里，Holon Reviewer 参与审阅了 Holon 仓库的 **763 个已合并 PR**，覆盖同期 804 个已合并 PR 的**约 95%**，在这些 PR 上累计提交 **1,529 次审阅**。

以上合并统计 `holonbot` 与本机 Reviewer 使用的个人 GitHub 账号 `jolestar`，排除 Copilot；覆盖的 PR 数按 PR 去重。

要做到“作者推新代码或 CI 跑完后自动接着审，而不是每次重新贴链接、重复发评论”，核心在于把审阅拆成三层机制：**模板角色契约、两层事件订阅、以及每条 PR 独立的 WorkItem**。

> 如果你想直接在自己的仓库里搭起这套流程，请参阅分步操作指南：[搭建持续跟进 PR 的本地 Code Reviewer](../guides/build-code-reviewer.md)。

## 1. 模板角色契约与权限确认

`code-reviewer` 模板在初始化 Agent 时会装入默认的工作规范（`AGENTS.md`）与配套 Skills（`code-review`、`github-review`、`ghx`、`sview`、`agentinbox`、`uxc`）。其中最关键的一条是**权限确认协议**：

```markdown
# Code Reviewer Agent

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

安装模板本身不会自动授予合并或写代码权限。在首次对话中，你只需用自然语言告诉它负责哪个仓库、是否允许自动合并以及哪些情况必须先升级询问，Agent 就会自己挂载仓库并把这些长期边界记入它私有的 `AGENTS.md`：

- **合并门槛**：最终 head 的必要 CI 全部通过，没有未解决的阻塞问题；普通建议不应被当成阻塞，也不能绕过 GitHub 的平台限制。
- **升级处理**：大规模重构、破坏性 API 变更或安全敏感修改交给操作者判断；未经授权，不主动代作者修代码。

## 2. 两层事件订阅与一条 PR 一个 WorkItem

要让 Reviewer 像团队成员一样自己盯住 PR，需要区分两类事件范围。`holon-reviewer` 通过 AgentInbox 采用两层订阅：

- **仓库订阅长期保留**，发现新开的 PR，让 reviewer 自动开始审阅。
- **每条 PR 单独跟进**，接收后续提交、CI 和审阅评论；合并或关闭后，清理该 PR 的订阅。

当新 PR 出现时，reviewer 会为这条 PR 创建独立的 [WorkItem](why-work-items.md)，把已审的 `head SHA`、阻塞问题与证据、已执行的本地验证以及当前等待条件持久化保存下来：

```text
已审版本：<head SHA>
阻塞问题：<问题、位置与依据>；或本次未发现阻塞问题
验证：<已执行的测试和结果>；<未覆盖的范围>
下一步：等待 <作者修复 / 当前 head 的 CI / 维护者决定>
```

## 3. 按版本复查，直到合并收尾

把状态保存在 WorkItem 中之后，后续每次 CI 完成或作者推送新提交，事件都会唤醒同一条 PR 的 WorkItem，期间不需要人工再发“请继续”：

- **精确对齐版本**：复查记录写明新的 `head SHA`，确认引用的 CI 属于当前版本，而不是上一轮提交的旧结果。
- **去重与增量反馈**：像前文 PR #2854 的第二版（`19f390ca`）那样，先逐项核对上一轮的阻塞问题是否已解决，已解决的不再重复刷屏；新引入的错误单独列出，让作者清楚下一步只需改哪里。
- **终态合并与清理**：当最终版本的必要检查全部通过、无遗留阻塞后，reviewer 按授权执行合并（或通知维护者合并），完成该 WorkItem 并退订这条 PR 的事件源；长期的仓库订阅则继续留给下一条新 PR。

## 下一步：动手搭建你的 Reviewer

想在自己的仓库里配置这套流程（只需在 Web GUI 中创建 Agent，再通过几句对话让它自动挂载代码库、记录权限并接通事件订阅），请直接阅读分步指南：

- **[搭建持续跟进 PR 的本地 Code Reviewer](../guides/build-code-reviewer.md)**

## 案例来源

文中的 PR #2854 使用了完整工作计划及 GitHub 提交、审阅、检查和合并快照。本次未重新执行案例中的测试，也未独立验证订阅清理结果。
