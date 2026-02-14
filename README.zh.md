# Holon

[English](README.md) | 中文

Holon 让 AI 编码 Agent 以全程无交互方式运行（默认使用 Claude Code），一步把 Issue 变成可合并的 PR 补丁和摘要——本地或 CI 都可用，无需人工值守。

设计方向：Holon 围绕“沙箱运行 + 标准化工件契约”构建，未来可以在其上逐步叠加更高层自动化（计划、补充信息询问、评审/合并控制器等）——分阶段目标。

## 为什么选择 Holon
- 默认无交互：无需 TTY/人肉输入，运行可重复、可预期。
- Issue → PR 端到端：拉取上下文、运行 Agent、一条命令创建/更新 PR。
- Patch-first 标准工件：始终产出 `diff.patch`、`summary.md`、`manifest.json`，便于审查、审计、CI 消费。
- 沙箱隔离：Docker + 快照工作区默认保护仓库，只有你主动选择才会写回宿主。
- 可插拔 Agent 与工具链：自由更换 Agent 引擎/Bundle，不改工作流。
- 本地/CI 同步体验：`holon solve` 本地或 GitHub Actions，输入输出一致。

## Agents
Holon 当前默认提供基于 Claude Code 的 Agent Bundle。你也可以通过 `--agent` / `HOLON_AGENT` 运行其他（包括自定义）Bundle，并通过 `--agent-channel` / `HOLON_AGENT_CHANNEL` 选择更新策略。

## GitHub Actions 快速开始（配合 holonbot）
1) 安装 GitHub App：[holonbot](https://github.com/apps/holonbot)。  
2) 添加触发 workflow（最小示例）：

```yaml
name: Holon Trigger

on:
  issue_comment:
    types: [created]
  issues:
    types: [labeled, assigned]
  pull_request:
    types: [labeled]

permissions:
  contents: write
  issues: write
  pull-requests: write
  id-token: write

jobs:
  holon:
    name: Run Holon (via holon-solve)
    uses: holon-run/holon/.github/workflows/holon-solve.yml@main
    with:
      issue_number: ${{ github.event.issue.number || github.event.pull_request.number }}
      comment_id: ${{ github.event.comment.id }}
    secrets:
      anthropic_auth_token: ${{ secrets.ANTHROPIC_AUTH_TOKEN }} # 必填输入
      anthropic_base_url: ${{ secrets.ANTHROPIC_BASE_URL }}
```

3) 配置密钥 `ANTHROPIC_AUTH_TOKEN`（确保该 repo 能访问），并通过 `secrets:` 映射传入，如上所示。`holon-solve` 会根据事件自动推导模式/上下文/输出目录并无交互运行 Agent。开箱即用：将 `examples/workflows/holon-trigger.yml` 复制到你的仓库即可快速触发。

可选：直接使用 Composite Action 并上传制品：
```yaml
- uses: holon-run/holon@v2
  with:
    ref: "${{ github.repository }}#${{ github.event.issue.number }}"
    anthropic_auth_token: ${{ secrets.ANTHROPIC_AUTH_TOKEN }}
    out_dir: holon-output
- uses: actions/upload-artifact@v4
  with:
    name: holon-output
    path: holon-output/
```

## 本地 CLI（`holon solve`）
前置条件：Docker、Anthropic token (`ANTHROPIC_AUTH_TOKEN`)、GitHub token (`GITHUB_TOKEN` 或 `HOLON_GITHUB_TOKEN` 或 `gh auth login`)，可选基础镜像（默认自动检测）。

安装：
- Homebrew：`brew install holon-run/tap/holon`
- 或下载发行版 tarball，将 `holon` 放入 `PATH`。

针对 Issue 或 PR 运行（自动收集上下文 → 运行 Agent → 发布结果）：
```bash
export ANTHROPIC_AUTH_TOKEN=...
export GITHUB_TOKEN=...   # 或使用 gh auth login

holon solve https://github.com/owner/repo/issues/123
# 或：holon solve owner/repo#456
```

行为说明：
- Issue：创建/更新分支和 PR，并附带 patch 与 summary。
- PR：将 patch 推送到 PR 分支，按需回复评论。

## 开发与文档
- 构建 CLI：`make build`；测试：`make test`；Agent Bundle：`(cd agents/claude && npm run bundle)`。
- Operator 指南（v0.11）：`docs/operator-guide-v0.11.md`
- `run` GA 契约：`docs/run-ga-contract.md`
- 架构设计：`docs/holon-architecture.md`
- Agent 契约：`rfc/0002-agent-scheme.md`
- 模式说明：`docs/modes.md`
- 贡献指南：`CONTRIBUTING.md`
