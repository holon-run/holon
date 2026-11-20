---
title: 在 Web GUI 中从模板创建首个专用 Agent 并跑通测试任务
summary: 在 Web GUI 中审阅并实例化角色模板，确认独立 agent_home 与预置 Skill 到位，并用一个只读测试任务完成初始化验收。
order: 10
---

# 在 Web GUI 中从模板创建首个专用 Agent 并跑通测试任务

本指南介绍如何在 Web GUI 中从已审阅的模板创建一个全新的专用 Agent，并通过一次最小只读测试任务确认它能够正常接活，同时不影响任何既有 Agent。

- 模板只负责在独立的 `agent_home` 中初始化新 Agent 的 `AGENTS.md` 角色契约与声明的 Skill，不会额外授予宿主机权限，也不会自动信任外部输入。机制细节见[Agent 模板](/zh-CN/concepts/agent-templates.md)。
- 本指南用于**新建独立角色实例**；如果要调整现有 Agent 的常驻规则或审批门禁，请参阅[创建并定制专属角色 Agent 与权限边界](customize-agent.md)。

## 前置条件

- Holon 运行时已启动，[Web GUI](use-web-gui.md) 可访问（默认 `http://127.0.0.1:7878/`），且已配置可用模型路由（见[配置模型](configure-models.md)）。
- 已在[模板参考](/zh-CN/reference/agent-templates.md)中选定并审阅过目标模板（文档中列出角色并不代表本地已下载安装）。
- 一个尚未被占用的语义化 Agent ID（例如 `reviewer-demo`）。

## 操作步骤

### 1. 在 Web GUI 中审阅或安装模板

打开 Web GUI 的 **Agent Templates** 页面（`/templates`），在创建实例前先确认模板的角色契约、依赖 Skill 与来源：

- 如果列表中已有 `code-reviewer`，直接点击查看其清单与绑定 Skill。
- 如果列表缺少该模板，点击 **Install Template** 并输入：

  ```text
  https://github.com/holon-run/holon/tree/main/agent_templates/code-reviewer
  ```

  如果想同步维护整个官方模板库，可在远程模板源中添加 `https://github.com/holon-run/holon` 并点击 **Sync Sources**。

也可以直接在主对话窗口让 `main` Agent 先帮你检查本地已有哪些可用模板，以及哪些 ID 尚未占用。

### 2. 创建独立 Agent 实例

以下两种 Web GUI 方式效果等价，任选其一即可：

- **在 `/templates` 页面创建**：选中已安装的 `code-reviewer` 条目，点击 **Create Agent**，填入空闲 ID `reviewer-demo`（若多个来源存在同名模板，本地安装选择器可能会带后缀）。
- **通过主 Agent 对话创建**：直接向 `main` 发送指令：

  > “请基于 `code-reviewer` 模板创建一个名为 `reviewer-demo` 的新 Agent。不要改动现有 Agent，也先不要挂载代码仓库。”

Holon 会为 `reviewer-demo` 初始化独立的 `agent_home`，写入对应的 `AGENTS.md`、记忆文件以及模板声明的 Skill。模板初始化绝不会覆盖非空目录。

### 3. 用最小只读任务完成初始化验收

在 Dashboard 列表中打开新建的 `reviewer-demo` 对话窗口，发送一条不涉及外部权限的只读核验指令：

> “阅读你当前的角色契约，列出你的常驻职责、已激活的 Skill 以及需要我审批的边界。不要访问 GitHub，也不要修改任何文件。”

核对两项结果：

1. Dashboard 中出现了独立的 `reviewer-demo` 实例。
2. 回复内容准确反映 `code-reviewer` 的角色职责与预期 Skill，且无需提前挂载仓库或提供 GitHub 凭据即可正常响应。

空闲状态的 Agent 不会持续消耗资源。验证通过后，即可按[创建并定制专属角色 Agent 与权限边界](customize-agent.md)为其挂载仓库并细化个人偏好与审批门禁。

## 故障排查

- **找不到模板**：在 `/templates` 确认本地安装后的选择器名称（重名时可能分配带后缀的 ID），或先通过 GitHub tree URL 安装。
- **Agent ID 已存在**：改用新的空闲 ID。切勿删除正在使用的目录或靠重套模板修改规则，调整现有 Agent 请使用[定制专属角色 Agent](customize-agent.md)。
- **Skill 引导安装失败**：查看 `/templates` 或 Dashboard 中的创建报错；修复网络或源访问后，运行 `holon agent repair reviewer-demo` 并复核 Skill 列表。
- **任务无法启动**：检查模型配置或运行 `holon config doctor`。

## 可选：CLI 创建与自定义模板源

在无头服务器或自动化脚本中，可使用终端命令完成创建与核验：

```bash
holon agent list
holon agent create reviewer-demo --template code-reviewer
holon agent get reviewer-demo --detail full
holon skills list --agent reviewer-demo
holon run --agent reviewer-demo "阅读你的角色契约，列出你的审查职责与批准边界。不要访问 GitHub，也不要修改任何文件。"
```

可通过 `holon agent get ... --detail full` 中的 `loaded_agents_md.agent_source.path` 与同目录下的 `template-provenance.json` 核查初始化来源。

<a id="when-the-built-in-catalog-is-not-enough"></a>

### 使用本地目录或自定义 GitHub 模板源

未指定 `--template` 时，`holon agent create` 会使用通用默认角色。如需直接从审阅过的本地目录或第三方仓库创建：

```bash
holon agent create my-agent --template /path/to/my-template
holon agent create my-agent --template https://github.com/owner/repo/tree/main/templates/my-template
```

模板目录结构、`template.toml` 与 `skills.toml` Schema 请参阅[Agent 模板参考](/zh-CN/reference/agent-templates.md)；自行编写模板请参阅[创建 Agent 模板](/zh-CN/guides/create-agent-template.md)。

## 另见

- [创建并定制专属角色 Agent 与权限边界](customize-agent.md)
- [为不同角色规划与隔离 Agent Skills](organize-agent-skills-and-roles.md)
- [安装、启用与更新 Skill](use-skills.md)
- [创建 Agent 模板](create-agent-template.md)
