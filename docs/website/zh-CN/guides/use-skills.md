---
title: 为 Agent 添加 Skill
summary: 找到 skill、安装到库、为某个 Agent 启用，并确认它已生效。
order: 18
---

# 为 Agent 添加 Skill

Skill 是一份 Agent 按需加载的 `SKILL.md` 工作流。Skill 放在共享的库里：先安装到
库，再按 Agent 启用。

## 前置条件

- daemon 在运行。

## 步骤

1. 看看库里已经有什么：

   ```bash
   holon skills catalog
   ```

2. 添加一个 skill：

   ```bash
   holon skills add /path/to/skill-dir
   holon skills add https://github.com/user/repo/tree/main/skills/my-skill --remote
   ```

3. 为某个 Agent 启用：

   ```bash
   holon skills enable my-skill --agent reviewer
   ```

4. 确认这个 Agent 能看到它：

   ```bash
   holon skills list --agent reviewer
   ```

## 确认成功

`holon skills list --agent reviewer` 里能看到这个 skill，之后遇到匹配的任务时
Agent 就能加载它。

## 保持 skill 最新

```bash
holon skills update
holon skills check
```

命令、来源规则和 `skills.toml` 的 schema 见 [Skills 参考](/zh-CN/reference/skills.md)。
