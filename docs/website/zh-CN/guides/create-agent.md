---
title: 从模板创建 Agent
summary: 挑选模板、创建 Agent 并确认其能正常接收处理任务。
order: 10
---

# 从模板创建 Agent

模板用于初始化新 Agent 的角色契约与可选 Skill。本指南以创建一个审查者（Reviewer）Agent 为例，演示如何在不影响已有 Agent 的前提下完成初始化。有关生命周期机制，请参阅 [Agent 模板](/zh-CN/concepts/agent-templates.md)。

## 开始之前

- 守护进程正在运行：`holon daemon start`。
- 已配置可用的模型连接；具体请参阅[配置模型](/zh-CN/guides/configure-models.md)。
- 一个未被占用的 Agent ID。创建前可运行 `holon agent list` 核对。
- 选定并审阅过具体模板。各角色契约请参阅[模板参考](/zh-CN/reference/agent-templates.md)；参考文档中列出的角色并不意味着本地已提前下载安装。

## 操作步骤

1. 在 Web GUI 中打开 **Agent Templates** 页面（`/templates`）。检查目标模板的角色契约、依赖的 Skill 以及模板来源。如果列表中缺少 `code-reviewer`，点击 **Install Template** 并输入：

   ```text
   https://github.com/holon-run/holon/tree/main/agent_templates/code-reviewer
   ```

   若需维护整个源仓库，可在远程源列表中添加 `https://github.com/holon-run/holon` 并点击 **Sync Sources**。等待同步任务完成并刷新模板目录。选中已安装的条目；若发生命名冲突，本地可能会分配不同的 install ID。

   当前暂无单独的 `holon templates` CLI 命令。纯命令行操作时，可以在 `--template` 中直接传入审阅过的本地模板目录或显式的 GitHub tree URL，如下所示。

2. 执行创建命令，将 `reviewer-demo` 替换为你自拟的空闲 ID，并将 `code-reviewer` 替换为已安装的选择器：

   ```bash
   holon agent create reviewer-demo --template code-reviewer
   ```

   若未指定 `--template`，Holon 会使用内置的通用默认模板。模板初始化严禁覆盖非空的 Agent Home。

3. 检查 Agent 与其关联的 Skill：

   ```bash
   holon agent list
   holon agent get reviewer-demo --detail full
   holon agent status reviewer-demo
   holon skills list --agent reviewer-demo
   ```

   在完整详情中通过 `loaded_agents_md.agent_source.path` 定位 `AGENTS.md`。核验该文件以及同目录下的 `template-provenance.json`（若存在），确认其角色定义与你所选的来源一致。

4. 分配一个小型只读测试任务：

   ```bash
   holon run --agent reviewer-demo "阅读你的角色契约，列出你的审查职责与批准边界。不要访问 GitHub，也不要修改任何文件。"
   ```

   这样无需预先接入真实代码仓库或 PR，即可直接验证初始化效果。

## 确认结果

Agent 出现在列表中，其 Agent Home 包含了选定的契约文件，且生效的 Skill 列表与模板依赖完全吻合。测试任务返回了符合角色设定的完成摘要。空闲状态下的 Agent 无需保持唤醒。

## 故障排查

- **模板未找到：** 在 GUI 中确认已安装的选择器名称，或使用绝对路径/GitHub tree URL。单纯指定角色名称不会自动触发下载安装。
- **Agent Home 已存在：** 更换一个全新未使用的 Agent ID。切勿删除已有目录或试图重新套用模板来覆盖定制；定制已有 Agent 请参阅[定制 Agent](/zh-CN/guides/customize-agent.md)。
- **Skill 引导安装失败：** 检查创建结果与报告缺失的源。修复访问权限或网络依赖后，运行 `holon agent repair reviewer-demo` 并再次核对 `holon skills list --agent reviewer-demo`。
- **任务无法执行：** 运行 `holon config doctor` 检查模型服务的连通性与配置。

<a id="when-the-built-in-catalog-is-not-enough"></a>

## 使用其他模板源

你可以使用空闲的 Agent ID，通过以下任一方式引用自定义模板：

```bash
holon agent create my-agent --template /path/to/my-template
holon agent create my-agent --template https://github.com/owner/repo/tree/main/templates/my-template
```

模板目录结构、选择规则以及 `template.toml` 和 `skills.toml` 格式说明请参阅 [Agent 模板参考](/zh-CN/reference/agent-templates.md)。编写自定义模板请参阅[创建 Agent 模板](/zh-CN/guides/create-agent-template.md)。
