---
title: holon solve
summary: 用 holon solve 在无头模式下自动处理 GitHub issue 和 pull request。
order: 8
---

# holon solve

`holon solve` 是用于自动处理 GitHub issue 和 pull request 的无头命令。给它一个目标，
它会运行一个 Agent 来收集上下文、实现修复、审查 PR 或发表评论，然后把结构化输出写入
产物目录，供脚本或 CI 使用。

## 何时使用 solve

在以下场景使用 `holon solve`：

- 无需手动操作 TUI 即可自动实现一个 GitHub issue
- 在 CI 中根据审查反馈修复 PR
- 对 pull request 跑一次审查
- 把 Holon 接入脚本化流水线（GitHub Actions、CLI 脚本）

交互式工作不要用 `holon solve`。需要直接和 Agent 对话时，用 `holon tui` 或 `holon run`。

## 目标引用格式

第一个位置参数是目标引用。Holon 接受三种格式：

| 格式 | 示例 | 说明 |
|--------|---------|-------|
| 完整 URL | `https://github.com/holon-run/holon/issues/42` | 最明确；类型从 URL 推断 |
| `owner/repo#NN` | `holon-run/holon#42` | 按 issue 或 pull request 处理 |
| `#NN` 加 `--repo` | `#42 --repo holon-run/holon` | 最短形式；需要 `--repo` |

## 快速开始

### 解决一个 issue

```bash
# 完整 URL 形式
holon solve https://github.com/holon-run/holon/issues/42

# 短形式
holon solve holon-run/holon#42

# 数字引用 + --repo
holon solve '#42' --repo holon-run/holon
```

Agent 会克隆仓库、读取 issue、收集相关上下文、实现修复并提交改动。输出产物写入临时目录。

### 提供自定义目标

```bash
holon solve holon-run/holon#42 \
  --goal "Add a --dry-run flag to the solve command"
```

用 `--goal` 覆盖 Agent 对目标的解读，或在 issue 正文很长时收窄工作范围。

### 审查一个 pull request

```bash
holon solve https://github.com/holon-run/holon/pull/753 \
  --goal "Review only: check for security issues and publish one review"
```

当目标里提到审查时，Holon 会加上护栏，要求 Agent 只发表一次审查或一条 PR 评论，然后停止。
这让审查运行保持单次且可预期。

### 根据审查反馈修复 PR

```bash
holon solve https://github.com/holon-run/holon/pull/753 \
  --base "feature-branch"
```

用 `--base` 为修复分支设置不同的基础分支。默认基础分支是 `main`。

## 配置

solve 读取标准的 Holon 运行时配置（`~/.holon/config.json`）。至少需要配置一个模型和凭据。
用 CLI 配置：

```bash
# 设置默认模型
holon config set model.default "anthropic/claude-sonnet-4-6"

# 安全地存储凭据（推荐）
holon config credentials set --kind api_key --stdin anthropic
# 粘贴 API key 并按回车，然后按 Ctrl+D
```

也可以用环境变量快速设置：

```bash
export ANTHROPIC_AUTH_TOKEN="your-api-key"
holon config set model.default "anthropic/claude-sonnet-4-6"
```

Holon 还需要一个 GitHub token。它会从环境中读取 `GITHUB_TOKEN` 或 `GH_TOKEN`：

```bash
export GITHUB_TOKEN="ghp_..."
holon solve holon-run/holon#42
```

确认配置完整：

```bash
holon config doctor
holon config models list
```

## solve 的工作方式

运行 `holon solve` 时，运行时内部会执行这些步骤：

1. **解析目标**：Holon 从你提供的引用中提取 owner、repo、issue/PR 编号和类型。

2. **准备目录**：创建输出目录（默认：`$TMPDIR/holon-output-<uuid>`），并用
   `github-context/` 子目录存放输入元数据。

3. **创建 Agent**：Holon 用配置的 solve 模板创建 Agent，通常是命令自带的
   `github-solver` 预设。它包含 `sview`、`code-review`、`github-issue-solve`、
   `github-pr-fix`、`github-review` 和 `ghx`。

4. **运行提示词**：运行时构造一个描述目标和 goal 的提示词，然后用配置的信任级别和
   轮次上限运行 Agent。

5. **收集产物**：运行结束后，Holon 把这些文件写入输出目录：

   | 文件 | 内容 |
   |------|---------|
   | `manifest.json` | 结果元数据：provider、status、outcome、target |
   | `summary.md` | Agent 所做工作的可读摘要 |
   | `run.json` | 完整的结构化运行响应 |

## 完整标志参考

```
holon solve <REF> [OPTIONS]
```

| 标志 | 类型 | 默认值 | 说明 |
|------|------|---------|-------------|
| `REF`（位置参数） | string | （必填） | GitHub URL、`owner/repo#NN` 或 `#NN` |
| `--repo` | string | — | 数字引用的仓库（例如 `holon-run/holon`） |
| `--base` | string | `main` | 修复分支的基础分支 |
| `--goal` | string | — | 覆盖 Agent 对目标的解读 |
| `--role` | string | — | 传给 Agent 的额外角色上下文 |
| `--agent` | string | `github-solve` | 要使用或创建的 Agent ID |
| `--template` | string | `github-solver` | Agent 使用的模板 |
| `--model` | string | — | 覆盖配置的模型（设置 `HOLON_MODEL`） |
| `--max-turns` | integer | — | 强制停止前的最大 Agent 轮次数 |
| `--authority-class`（别名 `--trust`） | string | `operator-instruction` | 本次运行的信任级别 |
| `--json` | flag | false | 以 JSON 而非文本打印输出 |
| `--home` | path | `~/.holon` | Holon home 目录 |
| `--workspace` | path | — | Agent 的工作目录 |
| `--cwd` | path | — | Agent 的当前工作目录 |
| `--input` | path | — | 输入上下文目录（覆盖默认值） |
| `--output` | path | — | 输出产物目录（覆盖默认值） |

## 与 holon run 的区别

`holon run` 和 `holon solve` 都在无头模式下执行 Agent，但用途不同：

| | `holon run` | `holon solve` |
|---|---|---|
| 用例 | 通用无头任务 | GitHub issue 和 PR |
| 输入 | 自由文本提示词 | GitHub 目标引用 |
| Agent 模板 | 未提供选择器时用隐藏的默认模板 | `github-solver`（预加载 GitHub skill） |
| 输出 | stdout 上的文本或 JSON | 输出目录中的结构化产物 |
| GitHub 集成 | 手动（`gh` CLI） | 自动收集上下文并分发 skill |
| 便于接入流水线 | 用 `--json` 获得结构化输出 | 内置 manifest 和摘要文件 |

通用自动化和脚本用 `holon run`。任务从 GitHub issue 或 pull request 开始、且你希望自动
选择 skill 时，用 `holon solve`。solve 预设是单次的：合并、批准或持续跟踪 PR 事件都
需要显式指令。

## 用 solve 写脚本

### 供脚本使用的 JSON 输出

```bash
holon solve holon-run/holon#42 --json | jq '.final_status'
# "completed"
```

### 自定义输出目录

```bash
holon solve holon-run/holon#42 --output ./solve-results/
cat ./solve-results/run.json
# { "provider": "holon-solve", "status": "completed", ... }
```


### CI 集成草图

```bash
#!/bin/bash
# 运行 solve，结果不完整时失败

OUTPUT=$(mktemp -d)
holon solve "$ISSUE_URL" --output "$OUTPUT" --json

STATUS=$(jq -r '.final_status' "$OUTPUT/run.json")
if [ "$STATUS" != "completed" ]; then
  echo "Solve did not complete: $STATUS"
  exit 1
fi

# Agent 可能已经提交了改动；把它们推上去
git push origin HEAD
```

### GitHub Actions

当你想在仓库工作流里用普通的步骤级环境变量传模型提供商凭据时，用这个复合 action：

```yaml
jobs:
  holon:
    runs-on: ubuntu-latest
    permissions:
      contents: write
      issues: write
      pull-requests: write
      id-token: write
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: holon-run/holon@main
        with:
          trigger: auto
          model: deepseek/deepseek-v3.2
        env:
          DEEPSEEK_API_KEY: ${{ secrets.DEEPSEEK_API_KEY }}
```

`trigger: auto` 会从受支持的 issue、pull request、label、assignment 和 `@holonbot` 评论
事件中推导目标和 goal。需要显式运行时，改用 `ref: owner/repo#123`。

`.github/workflows/holon-solve.yml` 里的可复用工作流仍然可用，作为兼容包装；但当你需要
任意提供商环境变量时，推荐入口是复合 action。

## 另见

- [Holon CLI 参考](/zh-CN/reference/cli)：完整命令树
- [配置参考](/zh-CN/reference/configuration)：模型和提供商设置
- [集成指南](/zh-CN/guides/integration)：HTTP 控制平面访问
- [多 Agent 协作](/zh-CN/guides/multi-agent)：创建和调用 Agent
