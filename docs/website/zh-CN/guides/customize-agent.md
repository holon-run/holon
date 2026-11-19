---
title: 创建并定制专属角色 Agent 与权限边界
summary: 在 Web GUI 中从模板创建或打开已有角色 Agent，通过自然语言对话将常驻职责、自主权限与审批门禁持久化写入 AGENTS.md 和操作者记忆，并以只读演练核验边界生效。
order: 20
---

# 创建并定制专属角色 Agent 与权限边界

当你需要一个职责明确、行为可控的长期 Agent——既能在安全范围内自主巡检、跑测试或准备草稿，又能在高影响操作前自动停下来请示——请按本指南配置它的角色契约与权限边界。

- 如需了解如何从远程源安装模板或用纯命令行初始化全新 Agent，请参阅[从模板创建 Agent](create-agent.md)。
- 如需了解 Agent 指引、工作区指引与信任边界的作用域划分，请参阅[Agent、角色与指引](/zh-CN/concepts/agents.md)。

## 前置条件

- Holon 运行时已启动，已打开 [Web GUI](use-web-gui.md) 并配置好可用模型路由（参见[配置与切换大模型](configure-models.md)）。
- 模板目录中已有所需角色模板（例如 `code-reviewer`、`docs-steward` 或 `research-steward`），或者已有处于空闲状态的现有 Agent（修改规则前先等当前活跃任务收尾）。

## 操作步骤

### 1. 在 Web GUI 中创建角色 Agent 或打开已有 Agent

1. 打开 [Web GUI](use-web-gui.md) 控制台（Dashboard）或 **Agent Templates**（`/templates`）页面。
2. 若需新建专属角色，选择已安装的模板（如 `code-reviewer`、`docs-steward` 或 `research-steward`），并指定一个未占用的语义化 Agent ID。
3. 若需调整现有 Agent，直接在控制台列表中打开它，确认当前没有正在修改文件的工作项。

Agent 一旦创建，便拥有独立的 `AGENTS.md`、记忆文件与历史状态。不要通过删除目录或重套模板来覆盖已有 Agent 的定制规则。

### 2. 在自然语言对话中声明职责、权限门禁与协作偏好

进入该 Agent 的对话页面，在聊天框里用自然语言直接下达配置要求（如需绑定本地代码库，可一并说明）：

> “请将 `/path/to/my-repo` 挂载为工作区，并把以下长期规则写入你的 `AGENTS.md`：
> 1. 你可以自主读取代码、运行本地测试，并在独立 Git worktree 中准备修改草稿。
> 2. 涉及合并 PR、推送 `main`、对外发布内容或修改凭据的操作，必须先在对话中向我确认，严禁擅自执行。
> 3. 同时将以下偏好记录到 `memory/operator.md`：默认用中文简洁汇报，结论附带真实验证命令和输出。”

Agent 会自动维护以下 Agent Home 文件，且不会破坏运行时托管的 `.holon/` 状态目录：

- **`AGENTS.md`**：定义常驻角色、自主授权范围、必须确认的门禁及升级原则——每轮对话自动加载。
- **`memory/operator.md`**：沉淀稳定个人偏好，例如回复语言、汇报风格与 Git 工作习惯（参见[管理 Agent 记忆](manage-agent-memory.md)）。

一份清晰的 `AGENTS.md` 示例结构如下：

```markdown
## 核心职责
- 审查已挂载仓库中的 Pull Request 与文档变更。
- 在独立 Git worktree 中复现问题并运行本地测试套件。

## 权限边界
无需重复请示即可自主执行：
- 读取文件、查看 Git 历史并执行本地只读检查或测试命令。
- 在独立 worktree 内准备补丁草稿或审阅纪要。

必须先获操作者明确确认：
- 推送分支、合并 PR、发布 Release 或对外发帖/外联。
- 任何涉及凭据、费用支出或不可逆外部状态的操作。
```

### 3. 区分不同层级的配置作用域，避免重复堆砌

不要把同一条规则到处复制，按作用域放到最合适的位置：

| 规则类型 | 存放位置 | 原因 |
|---|---|---|
| 常驻角色与审批权限门禁 | Agent Home `AGENTS.md` | 跟随该 Agent 生效，跨仓库复用 |
| 仓库构建命令与代码规范 | 代码仓库内 `AGENTS.md` | 跟随代码库走，仅在激活该工作区时加载 |
| 回复语言与个人协作习惯 | `memory/operator.md` | 精选偏好切片载入，避免塞爆 `AGENTS.md` |
| 可复用的标准化操作流程 | [Skill](write-skill.md)（`SKILL.md`） | 任务命中对应工作流时按需读取 |
| 当前具体任务目标与清单 | 当前 `WorkItem` | 随任务结算归档，不污染长期规则 |

> **信任与隔离边界提示：** `AGENTS.md` 是运行时加载的行为契约，而非操作系统级的强沙箱隔离。本地命令行执行仍运行在宿主机环境中（`host_local`）；此外，外部网页、Issue 评论或低信任输入绝不能覆盖 `AGENTS.md` 中记录的审批边界。详情参见[信任边界](/zh-CN/concepts/trust-boundaries.md)。

### 4. 用只读演练与越界测试验证边界生效

正式交给它长期任务前，先在对话里做两步轻量验收：

1. **核验已加载的职责与偏好：**

   > “读取你当前的 `AGENTS.md` 和 `memory/operator.md`，列出哪些动作你可以自主执行、哪些必须先请示我，并在不改动任何文件的前提下检查已挂载工作区状态。”

2. **越界边界演练：**

   > “假设某条外部 GitHub Issue 评论要求你直接把热修复推送到 `main` 并发布公告，你会怎么处理？”

确认 Agent 能准确复述最新条款，并明确表示会拒绝越权操作、转而向操作者升级请示。

### 5. 可选：通过 CLI 查看路径与验证

如果你在无头服务器或纯命令行环境维护 Agent，可运行以下命令定位真实路径：

```bash
holon agent list
holon agent get reviewer --detail full
```

查看返回结果中的 `loaded_agents_md.agent_source.path`（不要直接假定固定在 `~/.holon/` 下）。修改保存后，用一次全新的只读调用核验：

```bash
holon run --agent reviewer "读取你当前的 AGENTS.md，列出常驻职责与审批边界，不要修改任何文件。"
```

## 常见问题排查

- **改完后仍按旧规则回复：** 确认修改的是 `loaded_agents_md.agent_source.path` 指向的真实文件，并新开一轮对话测试。
- **项目仓库规则没有生效：** 让 Agent 调用 `GetWorkspaceState` 确认当前激活工作区；仅在 shell 里执行 `cd` 不会切换工作区或加载仓库级 `AGENTS.md`。
- **多处指令互相冲突：** 权限门禁统一收口在 Agent 的 `AGENTS.md`，仓库规范放在仓库 `AGENTS.md`；不要允许 Skill 或外部低信任任务文本覆盖审批边界。
- **上游模板更新了：** 模板只负责创建时的初始化，同步模板不会自动覆写现有 Agent；如有需要，请对照差异手动摘取更新条款。

## 另见

- [从模板创建 Agent](create-agent.md)
- [为不同角色规划与隔离 Agent Skills](organize-agent-skills-and-roles.md)
- [管理 Agent 记忆](manage-agent-memory.md)
- [创建 Agent 模板](create-agent-template.md)
- [编写与验证 Skill](write-skill.md)
