---
title: Skills
summary: Skill 来源、skills.toml schema，以及安装、启用、更新和校验 skill 的 CLI 命令。
order: 34
---

# Skills 指南

Skill 是可复用的本地工作流。一个 skill 以一个 `SKILL.md` 文件为根，描述一种
可重复的任务模式，Agent 需要时可以加载。

## 什么是 skill

一个 skill 通常包含：

- **用途** — 这个 skill 帮助完成什么任务
- **工作流** — 给 Agent 的分步指引
- **边界** — 这个 skill 该做什么、不该做什么
- **引用** — 要查看的相关文件或命令

Skill **不会自动生效**。运行时会提供一份可用 skill 的目录，只有当出现匹配的
任务时，Agent 才会打开对应的 `SKILL.md`。

## 位置

仓库内的 skill 通常放在：

```text
.codex/skills/<skill-name>/SKILL.md
skills/<skill-name>/SKILL.md
```

本仓库中的示例 skill 有：

- `ghx`
- `code-review`
- `github-issue-solve`
- `github-pr-fix`
- `github-review`

## Agent 如何使用 skill

1. 运行时提供一份 **skills 目录**，包含名称、描述和路径
2. 只有当 skill 匹配当前任务时，Agent 才会选择它
3. Agent 读取该 skill 的 `SKILL.md`
4. Agent 用普通的工具调用执行其中的工作流

这样能让 skill 保持显式，也避免加载无关的指引。

## 用 CLI 管理 skill

Holon 把 skill 管理分成两层：

- **Skill Library** — 已知 skill 的全局目录，用
  `holon skills add/remove/check/reconcile/catalog` 管理。可以把它当作你的
  本地 skill 注册表。
- **Agent Skills** — 按 Agent 启用的 skill，用
  `holon skills enable/disable/list` 管理。skill 必须先存在于 library，
  才能为某个 Agent 启用。

### Skill Library（全局）

Skill Library 是你本地的 skill 目录。用下面的命令管理：

#### 查看 library 目录

```bash
holon skills catalog
```

列出本地 Skill Library 中注册的所有 skill。

#### 向 library 添加 skill

```bash
# 从本地目录或 SKILL.md 文件添加
holon skills add /path/to/skill-dir

# 从远程源添加
holon skills add https://github.com/user/repo/tree/main/skills/my-skill --remote

# 把 skill 复制到用户目录，而不是引用它
holon skills add /path/to/skill --copy
```

#### 从 library 移除 skill

```bash
holon skills remove my-skill
```

#### 检查 library 一致性

```bash
# 对照 .skill-lock.json 检查所有 library 条目
holon skills check

# 检查某个具体 skill
holon skills check my-skill
```

> **注意：** 安装一个已安装的全局 skill 是幂等的：`holon skills add` 会跳过
> 安装并报告已有条目，不报错。在脚本和自动化里
> 可以放心重复执行。

#### 用 lock 文件校准 library

```bash
# 校准所有 library 条目
holon skills reconcile

# 校准某个具体 skill
holon skills reconcile my-skill
```

#### 从远程源更新 skill

```bash
holon skills update
```

拉取远程 skill 源并与 lock 文件（`.skill-lock.json`）比对。上游有变化的
skill 会被下载并更新。更新会保留 npx `skills` v3 的 lock 格式（source、
subdir、mode、etag），以便互通。

### Agent Skills（按 Agent）

skill 进入 library 后，再为具体 Agent 启用：

#### 列出某个 Agent 已启用的 skill

```bash
# 列出默认 Agent 的
holon skills list

# 列出某个具体 Agent 的
holon skills list --agent reviewer
```

列出该 Agent 当前已启用的所有 skill，包括名称、作用域（agent、workspace
或 user）和来源。

#### 为 Agent 启用 skill

```bash
# 为默认 Agent 启用
holon skills enable my-skill

# 为某个具体 Agent 启用
holon skills enable my-skill --agent reviewer

# 启用并复制到 Agent home
holon skills enable my-skill --copy
```

#### 为 Agent 禁用 skill

```bash
# 为默认 Agent 禁用
holon skills disable my-skill

# 为某个具体 Agent 禁用
holon skills disable my-skill --agent reviewer
```

> **兼容别名：** `holon skills install` 和 `holon skills uninstall` 仍被接受，
> 但会映射到新的 add/enable 和 remove/disable 模型。为了清晰，优先用新命令。

### Skill 的 `uses` 简写

添加 GitHub 上托管的 skill 时，可以用 `uses` 简写语法代替仓库 URL：

```bash
# 完整 URL 形式
holon skills add https://github.com/holon-run/holon/tree/main/skills/ghx --remote

# uses 简写，效果相同
holon skills add holon-run/holon/skills/ghx@main
```

`uses` 形式（`owner/repo/path@ref`）会在内部规范化为完整的 GitHub URL。它也
接受 `owner/repo/path#ref` 作为兼容输入。

### Skill 源类型

| 源 | 标志 | 示例 |
|--------|------|---------|
| 本地路径 | （默认） | `holon skills add ./skills/my-skill` |
| 远程 URL | `--remote` | `holon skills add https://... --remote` |

### 命令速查

| 层 | 命令 | 用途 |
|-------|---------|---------|
| Library | `holon skills catalog` | 列出 library 目录 |
| Library | `holon skills update` | 从远程源拉取并更新 skill |
| Library | `holon skills refresh` | 重新扫描本地根目录，刷新运行时目录 |
| Library | `holon skills add <source>` | 向 library 添加 skill |
| Library | `holon skills remove <name>` | 从 library 移除 |
| Library | `holon skills check [name]` | 检查 lock 文件一致性 |
| Library | `holon skills reconcile [name]` | 与 lock 文件校准 |
| Agent | `holon skills list [--agent]` | 列出已启用的 skill |
| Agent | `holon skills enable <name>` | 为 Agent 启用 |
| Agent | `holon skills disable <name>` | 为 Agent 禁用 |

> **注意：** `--builtin` 标志和模板清单里的 `kind = "builtin"` 已在 v0.26.0
> 移除。官方 Holon skill 现在和其他 GitHub 托管的 skill 用同样的方式引用。

## 用 HTTP 管理 skill

HTTP 控制平面把 library 操作和 Agent 操作分开：

### Library 端点

| 方法 | 路径 | 用途 |
|--------|------|---------|
| `GET` | `/api/skills/catalog` | 列出 library 目录 |
| `GET` | `/api/skills/catalog/{skill_id}` | 获取 skill 详情 |
| `POST` | `/api/skills/catalog/add` | 向 library 添加 skill |
| `POST` | `/api/skills/catalog/refresh` | 刷新运行时目录 |
| `POST` | `/api/skills/catalog/remove` | 从 library 移除 |
| `POST` | `/api/skills/catalog/reconcile` | 与 lock 文件校准 |
| `POST` | `/api/skills/catalog/check` | 检查一致性 |
| `POST` | `/api/skills/catalog/update` | 从远程源更新 skill |

### Agent 端点

| 方法 | 路径 | 用途 |
|--------|------|---------|
| `GET` | `/agents/:agent_id/skills` | 列出 Agent 的 skill |
| `POST` | `/control/agents/:agent_id/skills/enable` | 为 Agent 启用 |
| `POST` | `/control/agents/:agent_id/skills/disable` | 为 Agent 禁用 |

> **已弃用：** `POST /api/control/agents/:agent_id/skills/install` 和
> `POST /api/control/agents/:agent_id/skills/uninstall` 仍保留以兼容，但已被
> enable/disable 和 add/remove 取代。

### 非阻塞安装任务

通过 HTTP API 安装 skill（例如从 Web GUI）时，安装会作为后台任务运行，不会
阻塞请求。任务进度可在 `GET /api/jobs/{job_id}` 查看，并通过 localStorage
在页面重新加载后保留。

## TUI 集成

终端 UI 在 CLI 之外也提供 skill 管理：

- **斜杠命令** — 输入 `/skills` 查看 Agent 的 skill，`/skill-catalog` 浏览
  library，`/skill-add <source>` 添加到 library，`/skill-remove <name>` 移除，
  `/skill-enable <name>` / `/skill-disable <name>` 直接在聊天输入框里管理
  Agent 的启用状态。
- **skill 名称补全** — 使用斜杠命令时，TUI 会自动补全 skill 名称。
- **Agent 状态侧栏** — “Skills” 下的 Agent 详情视图列出所有可发现的 skill
  及其作用域（agent、workspace、user）。

有了这些 TUI 功能，你无需离开交互会话就能管理 skill。

## 如何写好一个 skill

让 skill 保持：

- **小巧** — 只聚焦一个工作流
- **可执行** — 给出 Agent 能照做的具体步骤
- **有边界** — 避免大范围覆盖项目的指令
- **持久** — 记录可复用的行为，而不是一次性的任务笔记

适合写成 skill 的主题：

- 解决 GitHub issue
- PR 评审流程
- 发布清单
- 事故分类
- 项目专属的测试/调试循环

不适合写成 skill 的主题：

- 临时会议记录
- 一次性的任务计划
- 大段照搬的文档

## 与 AGENTS.md 的关系

这样分工：

- **`AGENTS.md`** 放持久的角色、权限和本地约定
- **Skill** 放可复用的工作流
- **工作项** 放当前跟踪的目标

它们是不同的层：

| 面 | 用途 |
|---------|---------|
| `AGENTS.md` | 常驻指令和边界 |
| `SKILL.md` | 某一类任务的可复用工作流 |
| 工作项 | 当前目标和进度 |

## 另见

- [多 Agent 协作](/zh-CN/guides/multi-agent.md) — 把工作委托给子 Agent
- [工作项指南](/zh-CN/guides/work-items.md) — 跟踪持久目标
- [Web GUI](/zh-CN/guides/web-gui.md) — 在浏览器里管理 skill
- [TUI 指南](/zh-CN/guides/tui.md) — 在终端里管理 skill
- [运行时模型](/zh-CN/concepts/runtime-model.md) — skill 如何融入 Agent 的运行循环
