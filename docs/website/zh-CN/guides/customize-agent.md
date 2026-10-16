---
title: 定制 Agent 角色与指引
summary: 直接更新现有 Agent 的常驻指令，无需替换其 Home 目录或重新套用模板。
order: 20
---

# 定制 Agent 角色与指引

使用 Agent 专属的 `AGENTS.md` 来界定其长期职责与权限边界；使用项目工作区内的指引来规定跟随代码仓库走的局部约定。关于这两种作用域的区别，请参阅 [Agent、角色与指引](/zh-CN/concepts/agents.md)。

## 前置条件

- 确定一个你拥有管理权限的现有 Agent。
- 在修改指引之前，先等待其当前活跃任务执行完成。
- 对即将修改的文件保留备份或纳入版本管理。

## 操作步骤

1. 定位 Agent Home 并检查当前状态：

   ```bash
   holon agent list
   holon agent get reviewer --detail full
   holon agent status reviewer
   ```

   在上述命令中将 `reviewer` 替换为你实际的 Agent ID。响应中的 `loaded_agents_md.agent_source.path` 标明了该 Agent 真实的指引文件路径。使用该绝对路径进行编辑，不要假定所有环境都固定使用 `~/.holon`。

2. 编辑 Agent 的 `AGENTS.md`：
   使用你习惯的编辑器打开该文件，更新其立足角色、常驻职责或升级汇报规则：

   ```markdown
   # 代码审查员

   ## 核心职责
   - 审查代码变更的正确性、可读性与测试覆盖。
   - 在提出重构建议时，必须附带最小可复现用例或事实依据。

   ## 权限边界
   - 默认以只读方式运行；未经操作者明确授权不得提交代码或发布 Release。
   ```

3. 验证新指引是否被正确加载：

   ```bash
   holon agent get reviewer --detail full | grep -A 10 loaded_agents_md
   holon run --agent reviewer "简要列出你的常驻职责"
   ```

   核对输出是否已经准确体现了刚才更新的指引条款。

## 另见

- [Agent、角色与指引概念](/zh-CN/concepts/agents.md)
- [使用模板创建 Agent](create-agent.md)
- [管理 Agent 记忆](manage-agent-memory.md)
