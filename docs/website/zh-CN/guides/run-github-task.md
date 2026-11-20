---
title: 用 Holon 一次跑通或持续跟进 GitHub Issue 与 PR 修复
summary: 根据场景选择 Web GUI 对话式工作流（适合多轮 PR 反馈、CI 等待与独立 worktree 隔离）或单次执行的 holon solve（适合 CLI 与 CI 批处理），明确前置核验、发布门禁与收尾清理。
order: 90
---

# 用 Holon 一次跑通或持续跟进 GitHub Issue 与 PR 修复

本指南介绍如何根据任务周期选择合适的工作流，完成从环境核验、隔离修改到 PR 交付与工作树清理的 GitHub 协作闭环。任务执行完成绝不等于自动合并 PR。

## 先选对工作流

| 使用场景 | 推荐路径 | 原因 |
|---|---|---|
| 多轮 Review 修改、等待 CI 或持续跟进 PR | **Web GUI + 长驻 Agent** | 保留 `WorkItem` 进度，在关联 `git worktree` 中隔离改动，无需轮询即可等待 CI 与评审唤醒 |
| 专职持续评审多个 PR | [搭建持续跟进 PR 的本地 Code Reviewer](build-code-reviewer.md) | 直接复用 `code-reviewer` 契约与事件订阅闭环 |
| 单次限定范围的命令行修复或 CI 自动化 | **`holon solve`** | 使用 `github-solver` 预设单次运行并生成 `run.json`、`summary.md`、`manifest.json` 后退出 |

## 前置条件

- Holon 运行时已启动，并已配置可用模型（见[配置模型](configure-models.md)）。
- 已完成 GitHub 鉴权（本机 `gh` 已登录，或仅具备所需读/写最小权限的 `GITHUB_TOKEN` / `GH_TOKEN`——切勿打印到日志）。
- 本地已有仓库检出（或准备好待挂载路径）；授权改动前先处理掉与当前任务冲突的未提交变更。

## 路径 A：在 Web GUI 中通过对话持续推进 Issue 与 PR

对于需要多轮确认、跟踪 CI 或持续根据评审意见迭代的任务，直接在 [Web GUI](use-web-gui.md) 对话窗口启动即可，不必提前手动执行一长串终端诊断命令。

### 1. 对话完成自检并创建隔离 worktree

打开负责开发的 Agent（例如 `main`，或基于 `software-developer` 创建的专用开发 Agent），发送一段包含范围与边界的指令：

> “请挂载本地仓库 `/path/to/my-repo`。动代码前先自检模型连通性与 `gh auth status`，读取 issue `#42`（或 PR `#123`），并基于 `main` 创建独立的关联 worktree 分支（如 `fix/issue-42`）。只改动与该问题直接相关的文件，跑通受影响的测试；在 push 或创建 PR 前停下来请我确认（或仅创建 Draft PR，不要合并）。”

Agent 会自动挂载工作区、核验 GitHub 登录状态、调用 `CreateWorktree` 开出独立分支（不污染主目录），并遵守不自动合并的发布边界。详见[在项目工作区或 Git worktree 中工作](work-with-workspaces.md)。

### 2. 跟进 CI、评审回复与收尾清理

本地测试通过并批准发布 PR 后：

- **跨轮次等待 CI 与 Review**：让 Agent 通过[长生命周期工作项](run-long-lived-task.md)或[用 Agent Inbox 统一收口外部事件](connect-event-inbox.md)订阅仓库变更，仅在 CI 或新评论到达时唤醒处理。
- **验收交付物**：对照输出中的 PR 链接、最新 Commit SHA 与实测命令输出进行核验，不以口头摘要代替实证。
- **合并后清理**：PR 合入后，让 Agent 核实合并状态并安全清理临时 worktree（`RemoveWorktree`）与分支。

## 路径 B：使用 `holon solve` 执行单次 CLI / CI 任务

如果需要在终端或 CI 流水线中一次跑通单个任务，使用 `holon solve`。执行前请先确认本地检出干净（`git status --short --branch`）且 `holon config doctor`、`gh auth status` 正常；在目标检出目录执行或通过 `--workspace` 传入路径（`solve` 假定调用方已准备好本地目录，不承诺自动 clone）。

## 目标引用格式

推荐使用完整的 issue 或 PR URL，以便明确目标类型。同时也支持以下简写形式：

```text
owner/repo#42
#42 --repo owner/repo
```

在 Shell 中若以 `#` 开头请加上引号。将示例中的仓库名和编号替换为你的实际目标。

## 快速上手

### 解决一个 Issue

```bash
holon solve 'owner/repo#42' --workspace "$PWD"   --goal "实现 issue 42，运行相关测试并提交 PR。不要合并。"   --output ./solve-results/issue-42 --json
```

该目标授权了修改代码仓库与创建 PR。若希望进行先期只读调研，目标中必须显式注明仅限排查。采纳修改前，务必审查 `summary.md`、代码 Diff 与测试证据。

### 指定自定义目标

```bash
holon solve '#42' --repo owner/repo --workspace "$PWD"   --goal "调查该问题并提出修复方案。不要编辑、提交、推送或发布任何内容。"   --output ./solve-results/investigation-42 --json
```

目标中应清晰声明任务范围、验收检查项与允许的副作用。标准的 solve 工作流在需要代码改动时有能力执行 commit、push 以及创建/更新 PR；切勿寄希望于“未提及发布操作”就等同于“只读排查”。

### 评审 Pull Request

```bash
holon solve https://github.com/owner/repo/pull/123 --workspace "$PWD"   --goal "仅限审查：检查正确性与测试覆盖率。不要修改文件。发布一条 review 或 PR 评论后停止。不要批准，不要合并。"   --output ./solve-results/review-123 --json
```

审查护栏会要求 Agent 在发布前检查最新提交与已有评审，使用单一发布界面，并在重试前核实模糊的返回结果。执行后依然需要查看发布的评论链接与审查的 commit SHA。若执行在发布后被中断，重新运行前先在 GitHub 上检查状态。

### 根据评审反馈修复 PR

```bash
holon solve https://github.com/owner/repo/pull/123 --workspace "$PWD"   --goal "处理可操作的评审反馈，运行受影响的测试，并更新该 PR。汇报已处理的每一项内容及任何阻塞项。不要合并。"   --output ./solve-results/fix-123 --json
```

若 Agent 需要基准分支提示，可传入 `--base BRANCH`。该参数不能替代对 PR 实际基准与最新提交的核查。

## solve 的工作原理

运行时解析目标、准备输入/输出目录、选用配置的 solve 模板（通常为 `github-solver`），并启动限定范围的提示词执行。每次运行建议使用独立的输出目录，避免将历史产物误判为新结论。

运行结束后，检查以下文件：

| 文件 | 检查要点 |
|---|---|
| `run.json` | 结构化运行结果，包含 `final_status` |
| `summary.md` | 改动点、验证记录、发布链接以及遗留的阻塞项 |
| `manifest.json` | 执行结论元数据与可用产物引用 |

在早期配置阶段失败可能无法生成完整的产物集。请保留标准错误输出与命令退出码。[CLI 参考](../reference/cli.md)列出了全部参数；本指南不重新定义输出结构。

## 与 holon run 的区别

针对通用的无头提示词任务使用 `holon run`。当任务以 GitHub issue 或 PR 为输入起点、且需要 solve 预设自带的 GitHub 工作流时，使用 `holon solve`。无论是进程成功退出还是生成了简短摘要，都不能替代对预定验收标准的逐一核对。

## 脚本化集成

### 用于脚本的 JSON 输出

在检查 `final_status` 前先捕获命令退出码。避免在不保留退出码的情况下直接通过管道将输出传递给 `jq`。

### 自定义输出目录

```bash
holon solve 'owner/repo#42' --workspace "$PWD"   --goal "仅限调查；不要编辑或发布。"   --output ./solve-results/inspect-42 --json
cat ./solve-results/inspect-42/summary.md
```

### CI 集成范例

以下 Bash 示例在遇到执行错误或未完成运行时会立即中止，且不会盲目推送当前检出的分支：

```bash
set -euo pipefail
: "${TARGET_REF:?请设置 issue 或 PR 引用}"
OUTPUT=$(mktemp -d)

if holon solve "$TARGET_REF" --workspace "$PWD"   --goal "仅限调查；不要编辑、提交、推送或发布。"   --output "$OUTPUT" --json >"$OUTPUT/stdout.json"; then
  jq -e '.final_status == "completed"' "$OUTPUT/run.json" >/dev/null
  cat "$OUTPUT/summary.md"
else
  status=$?
  printf 'Solve 失败；请检查 %s 中的产物
' "$OUTPUT" >&2
  exit "$status"
fi
```

即使执行失败，在排查敏感信息后也应将该目录作为 CI 产物归档。添加具体的业务验收检查；`final_status=completed` 仅代表运行时本身执行完毕。

### GitHub Actions 集成

仓库中的组合 Action（composite action）支持传入目标 `ref` 或在受支持的事件中传入 `trigger: auto`。在启用发布权限前，先审查检入的 `action.yml` 与工作流的 Token 权限。在工作流密钥管理中保存凭据。遵循仓库安全规范固定 Action 版本。

在任务超时后不要盲目自动重试具备发布权限的工作流：先在 GitHub 上确认它是否已经创建了 Commit、PR、Review 或评论。

## 任务未完成时的排查

检查模型连通性、GitHub 鉴权状态、选中的检出目录以及执行的标准错误。若 Agent 达到了轮次上限或时间限制，在决定继续或重新启动前，先检查已生成的产物与外部副作用。如实报告未解决的阻塞项，切勿谎称问题已解决。

## 另见

- [搭建持续跟进 PR 的本地 Code Reviewer](build-code-reviewer.md)
- [在项目工作区或 Git worktree 中工作](work-with-workspaces.md)
- [用 Agent Inbox 统一收口外部通知与异步事件](connect-event-inbox.md)
- [运行长生命周期任务](run-long-lived-task.md)
- [CLI 参考](../reference/cli.md)
- [故障排查](troubleshooting.md)
