---
title: Skills
summary: Skill 来源规范、管理命令以及用于安装、启用、更新和校验的 API 端点。
order: 34
---

# Skills

本页记录 Skill 的来源规范、管理命令与 API 端点。
具体操作指引请参阅[安装与更新 Skill](/zh-CN/guides/use-skills.md)以及[编写 Skill](/zh-CN/guides/write-skill.md)。

## 什么是 skill

关于发现机制、加载方式与执行权限边界，请参考[Skill 与工具](/zh-CN/concepts/skills-and-tools.md)。入口文件统一为 `SKILL.md`。

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

目录暴露名称、描述与路径；当任务匹配时，Agent 会读取 `SKILL.md`。
请遵循[编写与校验 Skill](/zh-CN/guides/write-skill.md)来检查加载与执行行为。

## 用 CLI 管理 skill

Library 命令管理共享注册表；enable/disable 命令用于调整具体 Agent 的启用项。
[管理指南](/zh-CN/guides/use-skills.md)详细介绍了“安装 → 启用 → 校验”的工作流。

### Skill Library（全局）

本节命令用于管理共享的 Library 状态。

#### 查看 library 目录

`holon skills catalog` 列出所有已注册条目。

#### 向 library 添加 skill

`holon skills add <source> [--remote] [--skill <name>] [--copy]` 支持本地目录、`SKILL.md` 文件、远程 URL 或下文介绍的 GitHub 简写形式。
`--skill` 用于从包含多个 Skill 的来源中挑选具体一项。本地安装默认采用软链接；`--copy` 会在本地创建独立受管副本。

GitHub 远程 skill 会打包下载为单个仓库归档并在本地进行探测，无需依赖 GitHub
Contents/Tree API。归档内容会计算校验和并生成稳定的文件夹哈希，供 `holon skills update` 使用。

#### 从 library 移除 skill

`holon skills remove <name>` 从 Library 中移除条目。

#### 检查 library 一致性

`holon skills check [name]` 对照 `.skill-lock.json` 检查一项或全部条目。

> **注意：** 安装一个已安装的全局 skill 是幂等的：`holon skills add` 会跳过
> 安装并报告已有条目，不报错。在脚本和自动化里
> 可以放心重复执行。

#### 用 lock 文件校准 library

`holon skills reconcile [name]` 根据 lock 文件状态对齐单项或全部条目。
在接受变更内容前，建议先核对差异。

#### 从远程源更新 skill

`holon skills update [name]` 拉取远程源并与 lock 文件（`.skill-lock.json`）比对。发生上游变动的条目会被下载更新。更新过程遵循 npx `skills` v3 lock 规范（source、subdir、mode、etag）以保证兼容性。

### Agent Skills（按 Agent）

使用 `--agent <id>` 指定目标 Agent。在 Agent 执行模式下，省略该参数代表当前调用者；在 Operator 模式下则默认指向 default agent。

#### 列出某个 Agent 已启用的 skill

`holon skills list [--agent <id>]` 显示当前生效的 Skill，包含名称、作用域（agent、workspace 或 user）及来源。

#### 为 Agent 启用 skill

`holon skills enable <name> [--agent <id>] [--copy]` 为指定 Agent 启用已知 Skill。
`--copy` 会在 Agent Home 中放入独立的受管副本。

#### 为 Agent 禁用 skill

`holon skills disable <name> [--agent <id>]` 移除 Agent 层面的启用配置。

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

请参考[编写与校验 Skill](/zh-CN/guides/write-skill.md)获取最小化的 `SKILL.md` 模板、测试任务以及发现与加载检查。每个 Skill 应当专注于单一的可复用工作流，例如代码评审、发布检查清单、故障分流排查或特定的测试调试循环。临时计划和复制来的大段文档不应作为 Skill。

## 与 AGENTS.md 的关系

请分别参阅 [Agent 与角色契约](/zh-CN/concepts/agents.md)、[Skill 与工具](/zh-CN/concepts/skills-and-tools.md)以及 [WorkItem 与 Task](/zh-CN/concepts/work-items-and-tasks.md)了解长期角色指引、复用工作流与跟踪目标的职责分工。

## 另见

- [多 Agent 协作](/zh-CN/guides/delegate-work.md) — 把工作委托给子 Agent
- [工作项指南](/zh-CN/reference/work-items.md) — 跟踪持久目标
- [Web GUI](/zh-CN/guides/use-web-gui.md) — 在浏览器里管理 skill
- [TUI 指南](/zh-CN/reference/tui.md) — 在终端里管理 skill
- [运行时模型](/zh-CN/concepts/runtime-model.md) — skill 如何融入 Agent 的运行循环
