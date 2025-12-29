# Holon

[English](README.md) | 中文

Holon 让 AI 编码代理以全程无交互方式运行，一步把 Issue 变成可合并的 PR 补丁和摘要——本地或 CI 都可用，无需人工值守。

## 为什么选择 Holon
- 默认无交互：无需 TTY/人肉输入，运行可重复、可预期。
- Issue → PR 端到端：拉取上下文、运行代理、一条命令创建/更新 PR。
- Patch-first 标准工件：始终产出 `diff.patch`、`summary.md`、`manifest.json`，便于审查、审计、CI 消费。
- 沙箱隔离：Docker + 快照工作区默认保护仓库，只有你主动选择才会写回宿主。
- 可插拔代理与工具链：自由更换代理引擎/Bundle，不改工作流。
- 本地/CI 同步体验：`holon solve` 本地或 GitHub Actions，输入输出一致。

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
    secrets: inherit
```

3) 配置密钥 `ANTHROPIC_AUTH_TOKEN`。`holon-solve` 会根据事件自动推导模式/上下文/输出目录并无交互运行代理。

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

针对 Issue 或 PR 运行（自动收集上下文 → 运行代理 → 发布结果）：
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
- 构建 CLI：`make build`；测试：`make test`；代理 Bundle：`(cd agents/claude && npm run bundle)`。
- 架构设计：`docs/holon-architecture.md`
- 代理契约：`rfc/0002-agent-scheme.md`
- 模式说明：`docs/modes.md`
- 贡献指南：`CONTRIBUTING.md`
