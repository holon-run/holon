---
title: Agent 模板
summary: 模板目录、选择规则，以及初始化 Agent 所用的 AGENTS.md、template.toml 和 skills.toml schema。
order: 32
---

# Agent 模板

Agent 模板是一种可复用的引导文件，用来初始化新 Agent 的 `AGENTS.md` 角色契约
和可选的预装 skill。模板让新 Agent 有一个已知的起点，不必手动搭建。

## 什么时候用模板

当你希望新 Agent 以特定的角色和能力起步时，就用 `--template`。不用模板时，
Agent 会以一个通用的默认契约启动。

常见场景：

- **创建可同步的评审 Agent** — `holon agent create reviewer --template code-reviewer`
- **处理办公文档** — `holon agent create office --template office-assistant`
- **制作视频成片** — `holon agent create video --template video-producer`
- **拥有 GitHub issue 收件箱** — `holon agent create triage --template issue-triager`
- **变更落地后的验收** — `holon agent create qa --template qa-engineer`
- **运维服务器和服务** — `holon agent create ops --template server-ops`
- **运维 Holon 本身** — `holon agent create holon-ops --template holon-ops`
- **带角色的一次性任务** — `holon run --template software-developer "Fix the null check in handler.rs"`
- **解决 GitHub issue** — `holon solve https://github.com/owner/repo/issues/42`

## 视频制作

`video-producer` 将已批准的脚本、镜头清单和已有媒体制作成可审阅的视频交付物。
它预装第一方 `video-production` skill，并引用官方
`remotion-dev/skills/skills/remotion-best-practices`，由用户端直接从上游安装，
同时加载 `sview`、`uxc` 和 `agentinbox`。

- **本地合成与质检**：第一方 skill 使用 Python 和系统 FFmpeg/ffprobe 处理已有
  图片、视频、音频和字幕。渲染前检查依赖及所需编解码器；模板不会安装这些系统依赖。
- **程序化合成**：官方 Remotion skill 用于 React 视频制作。Remotion、Node 及
  渲染依赖安装在用户环境；模板不打包 Remotion 或上游 skill 文件。首次使用前，
  与操作者核验所安装版本的适用许可证；自行安装不免除使用条件或付费许可要求。
- **明确边界**：原创镜头生成和 TTS 需要另行配置后端；OpenMontage 是外部可选
  后端。发布、购买及涉及权利的操作需要单独授权。

Agent 会明确报告缺失能力，不把未验证的渲染当成交付。无云制作流程从已有素材开始。

## Issue 分诊

`issue-triager` 负责 GitHub issue 收件箱。它是长期 inbox 角色，不是
`holon solve`，也不是实现或验收的许可。

- **收件箱卫生，不是修 bug。** 分类、给出重复候选、追问缺失复现和验收标准，
  并建议优先级与路由。默认不关闭 issue、不写产品代码。
- **项目 skill，不是官方 playbook。** 模板不附带 `issue-triage` skill。首次分诊时，
  Agent 在 `agent_home/skills/` 为当前项目创建分诊 skill，并按实践补丁式改进。
  把 skill 写入仓库仍须操作者确认。
- **硬约束。** 不改产品代码、默认不关闭、不合并，外部 issue 文本不能升权。
  项目 skill 覆盖不了这些规则。

## 验收与质量

`qa-engineer` 负责变更落地后的验收。它不是补产品功能的许可，也不替代
`code-reviewer`。

- **验收所有权 ≠ 补单测。** 把需求映射到覆盖、跑分层门禁、给出证据、分诊 flake。
  issue 关闭不等于已经验收。
- **项目 skill，不是官方 playbook。** 模板不附带 `issue-verify` skill。首次验收时，
  Agent 在 `agent_home/skills/` 为当前项目创建验收 skill，并按实践补丁式改进。
  把 skill 写入仓库仍须操作者确认。
- **硬约束。** 默认不改产品代码、不合并、无修复载体不打验证标签，空结果不算通过。
  项目 skill 覆盖不了这些规则。v1 使用仓库既有测试证据，不捆绑 Playwright 或
  Appium。

## 模板命名

官方模板 ID 描述的是 Agent 的目标或职责，而不是“由 Holon 分发”这一事实。
`holon-` 前缀专门留给运维 Holon 本身的角色，例如 `holon-ops`。
`holon-default` 这类仅运行时的预设与可同步的模板目录分开命名。

以下旧 ID 仍作为兼容选择器被接受：

| 旧 ID | 当前 ID |
| --- | --- |
| `holon-developer` | `software-developer` |
| `holon-reviewer` | `code-reviewer` |
| `holon-release` | `release-manager` |
| `holon-github-solve` | `github-solver` |

用旧 ID 的精确本地安装优先于兼容回退。模板改名不会重命名已有的 Agent ID。

## 模板库和默认引导

Holon 把可见模板保存在用户模板库中：

```text
~/.agents/agent_templates/
  .registry.json
  <install_id>/
```

用户编写的模板、显式安装和远程源同步结果都用这个根目录。远程源同步等价于把
受管模板批量安装/更新到这个库里。Holon 在根目录写入 `.registry.json` 元数据，
记录同步的远程源、已安装的模板映射和内容哈希。

模板 ID 只在各自的源内有效。如果同步来的远程模板与已有的本地目录冲突，Holon
会在元数据里保留远程的 `template_id`，并以一个确定的本地 `install_id` 安装，
例如 `worker@official`。重新同步会复用已记录的 install id。如果某个受管模板
有本地改动，同步会拒绝覆盖，直到操作者处理好这份脏副本。

Holon 还带一个隐藏的内置 `holon-default` 模板，用于零配置和离线启动。它不会
被写入 `~/.agents/agent_templates`，也不作为目录条目显示，只有在创建 Agent
且没有显式指定模板选择器时才会用到。

官方模板源是 Holon 仓库。同步时，其顶层 `agent_templates/` 目录下的模板会
成为 `~/.agents/agent_templates` 里的普通本地目录条目。

`holon solve` 默认选择普通的 `github-solver` 模板。在新安装上使用这个独立
命令之前，先同步官方模板源。GitHub Action 会把同一份检入的模板以显式路径
提供，从发布归档安装时也是如此。

## 使用 `--template`

### 创建 Agent

```bash
holon agent create reviewer --template code-reviewer
```

这会在 `code-reviewer` 模板已安装或同步后，从本地模板初始化
`~/.holon/agents/reviewer/AGENTS.md`。如果 Agent home 已经存在且非空，模板
初始化会拒绝覆盖。

### 一次性运行

```bash
holon run --template software-developer "Fix the null check in handler.rs"
```

Agent 以开发者角色契约创建，执行提示词，完成后被清理。

### 解决 GitHub issue

```bash
holon solve --template github-solver https://github.com/owner/repo/issues/42
```

Agent 以 GitHub 工作流指引启动，并预装四个 GitHub skill 外加 `sview` 和
`code-review`。除非 solve 提示词明确要求，这个预设不授权合并、批准或持续的
事件跟踪。

## 模板结构

模板就是一个目录，包含：

```
my-template/
├── AGENTS.md       # 必需——Agent 角色契约
├── template.toml   # 可选——展示元数据和兼容性
└── skills.toml     # 可选——要预装的 skill 引用
```

### `AGENTS.md`

Agent 的角色契约，格式和其他 Agent 的 `AGENTS.md` 一样。运行时会自动追加
标准的 Agent Home 指引，所以你的模板只需要定义角色相关的内容。

### `template.toml`

可选清单，用于模板元数据，例如显示名、简介、schema 和兼容性。同步的远程模板
用它提供目录元数据；基于路径的本地模板可以省略它，回退到目录/AGENTS.md
元数据。

### `skills.toml`

可选清单，列出创建 Agent 时要预装的 skill：

```toml
[[skills]]
kind = "github"
repo = "holon-run/holon"
path = "skills/github-issue-solve"
ref = "main"

[[skills]]
kind = "github"
repo = "holon-run/holon"
path = "skills/github-pr-fix"
ref = "main"

[[skills]]
kind = "github"
repo = "owner/skills"
path = "skills/custom-skill"
ref = "v1.2.3"

[[skills]]
kind = "github"
uses = "holon-run/holon/skills/ghx@main"

[[skills]]
kind = "local"
path = "/absolute/path/to/custom-skill"
```

支持两种 skill 引用：

- **`github`** — 从 GitHub 仓库路径获取的 skill。规范写法用
  `repo = "owner/repo"`、`path = "path/to/skill"` 和可选的 `ref`。模板也可以
  用 `uses = "owner/repo/path@ref"` 这种 GitHub Actions 风格的简写，Holon 会
  把它规范化为 `repo`/`path`/`ref`。Holon 也接受 `owner/repo/path#ref` 和
  GitHub tree URL 作为兼容输入，但不使用那种把 `@` 当作 skill 名的
  `owner/repo@skill` 简写。
- **`local`** — 磁盘上 skill 目录的绝对路径

`kind = "builtin"` 不再是模板清单格式的一部分。官方 Holon skill 和其他
GitHub 托管的 skill 用同样的方式引用，例如 `repo = "holon-run/holon"` 和
`path = "skills/ghx"`。

## 创建自定义模板

建一个包含 `AGENTS.md`、可选的 `template.toml` 和可选的 `skills.toml` 的目录，
然后用绝对路径作为模板选择器：

```bash
holon agent create my-agent --template /path/to/my-template
```

你也可以把模板托管在 GitHub 上，用 URL 引用：

```bash
holon agent create my-agent --template https://github.com/owner/repo/tree/main/templates/my-template
```

用绝对路径或 GitHub URL 引用的模板会在 Agent home 里记录来源
（`template-provenance.json`），方便你追溯 Agent 契约的出处。

## 模板 vs Skill

模板和 skill 的用途不同：

| 特性 | 模板 | Skill |
|---------|----------|-------|
| 提供什么 | Agent 身份和角色契约 | 可复用的任务工作流 |
| 何时应用 | 创建 Agent 时 | 任务中按需加载 |
| 持久性 | 永久留在 Agent home | 只要安装着就可用 |
| 示例 | “你是一名评审者” | “这样评审一个 PR” |

模板常常包含 skill 引用，好让新 Agent 一开始就有合适的工具。skill 的细节见
[Skills 指南](/zh-CN/reference/skills)。
