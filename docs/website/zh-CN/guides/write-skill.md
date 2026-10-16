---
title: 编写与验证 Skill
summary: 编写轻量级 SKILL.md 工作流，为测试 Agent 启用并验证其加载与遵循情况。
order: 50
---

# 编写与验证 Skill

本指南通过一个只读提取 Markdown 文档大纲的工作流，演示如何编写并验证 Skill。立足角色的常驻指引应放在 `AGENTS.md` 中；两者的区别详见 [Skill 与工具概念](/zh-CN/concepts/skills-and-tools.md)。

## 前置条件

- 一个用于测试的现有 Agent 以及正常的模型连接。
- 具备创建本地目录并为该 Agent 启用 Skill 的操作权限。
- 一个可供 Agent 读取的测试 Markdown 文档。

## 操作步骤

1. 创建独立的 Skill 目录：

   ```bash
   mkdir -p "$HOME/holon-skill-examples"
   mkdir "$HOME/holon-skill-examples/doc-outline"
   ```

2. 编写 `doc-outline/SKILL.md`：

   ```markdown
   ---
   name: doc-outline
   description: 读取指定的 Markdown 文档，提取并汇报其标题大纲与表述不清的说明，禁止修改文件。
   ---

   # 文档大纲提取

   当操作者要求梳理 Markdown 文档大纲或排查结构时使用本工作流。

   ## 工作步骤
   1. 使用文件读取工具查看目标 Markdown 文件的全文。
   2. 按层级列出所有标题（#、##、###）。
   3. 标出任何缺乏前置条件或步骤缺失的不完整章节。
   4. 以 Markdown 列表形式输出大纲总结，不要修改原文件。
   ```

3. 将该 Skill 添加至全局技能库并为测试 Agent 启用：

   ```bash
   holon skills add "$HOME/holon-skill-examples/doc-outline"
   holon skills enable doc-outline --agent tester
   holon skills list --agent tester
   ```

   确认输出列表中包含 `doc-outline` 且处于已启用状态。

4. 验证 Agent 的加载与遵循情况：
   发送一条匹配该 Skill 描述的任务：

   ```bash
   holon run --agent tester "使用 doc-outline 工具梳理 README.md 的大纲结构"
   ```

   检查输出是否严格遵循了 `SKILL.md` 规定的提取逻辑与只读限制。

5. 清理测试资源：
   测试完成后禁用该 Skill 并从技能库中移除：

   ```bash
   holon skills disable doc-outline --agent tester
   holon skills remove doc-outline
   ```

## 另见

- [Skill 与工具概念](/zh-CN/concepts/skills-and-tools.md)
- [安装、启用与更新 Skill](use-skills.md)
- [Skill 参考](/zh-CN/reference/skills.md)
