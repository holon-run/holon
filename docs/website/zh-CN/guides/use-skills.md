---
title: 安装、启用与更新 Skill
summary: 管理共享 Skill Library 并核实指定 Agent 的可用 Skill 列表。
order: 40
---

# 为 Agent 添加 Skill

Skill 是 Agent 在需要时按需加载的 `SKILL.md` 工作流。Skill 统一保存在共享库中：只需安装到 Skill Library 一次，之后按 Agent 进行独立启用。
有关加载机制与权限边界，请参阅 [Skill 与工具](/zh-CN/concepts/skills-and-tools.md)。

## 开始之前

- Holon 守护进程正在运行。
- 目标 Agent 已存在；在下文命令中将 `reviewer` 替换为你实际的 Agent ID。
- 来源可信且你已审阅过其 `SKILL.md`。安装 Skill 不会自动授予系统工具权限，也不会自动安装其中提及的外部依赖程序。

## 操作步骤

1. 检查已注册的 Skill：

   ```bash
   holon skills catalog
   ```

2. 使用支持的来源格式添加 Skill：

   ```bash
   holon skills add /path/to/skill-dir
   holon skills add https://github.com/user/repo/tree/main/skills/my-skill --remote
   ```

   将示例路径替换为你的实际来源。本地 Skill 默认采用软链接；仅在需要独立受管副本时使用 `--copy`。针对 GitHub 源，支持 `owner/repo/path@ref` 简写：

   ```bash
   holon skills add holon-run/holon/skills/ghx@main
   ```

   在后续命令中使用目录输出中报告的 Skill 名称。

3. 为目标 Agent 启用该 Skill：

   ```bash
   holon skills enable my-skill --agent reviewer
   ```

4. 验证启用状态：

   ```bash
   holon skills list --agent reviewer
   ```

5. 给该 Agent 分配一个匹配该 Skill 的小型测试任务，显式要求其先阅读 `SKILL.md`。测试范围应保持在 Skill 的前置要求与你的授权范围内。查看执行轮次：

   ```bash
   holon transcript --agent reviewer --limit 30
   ```

## 确认结果

执行 `holon skills list --agent reviewer` 应展示预期的来源与作用域。
测试执行记录中应显示 Agent 在使用该工作流前已成功读取了 `SKILL.md`。仅在目录中可见并不证明工作流已被实际加载或执行。

## 保持 Skill 最新

```bash
holon skills update my-skill
holon skills check my-skill
holon skills list --agent reviewer
```

更新操作会依据记录的远程源元数据拉取最新内容。软链接形式的本地 Skill 会直接反映源目录的修改；执行 `update` 不会去拉取该本地源码仓库。省略名称则检查或更新全部条目。由于这些是共享库操作，更新依赖项前请评估对其他 Agent 的潜在影响。

## 禁用或移除 Skill

```bash
holon skills disable my-skill --agent reviewer
holon skills list --agent reviewer
```

若仅希望特定 Agent 停用该 Skill，使用禁用命令。
执行 `holon skills remove my-skill` 会从全局 Library 中彻底移除该条目，可能影响其他 Agent；操作前请核查依赖方。如果其他发现作用域中存在同名 Skill，它可能仍会出现在最终生效列表中。

## 故障排查

- **找不到 Skill 名称：** 查看 `holon skills catalog` 输出；请以 catalog 报告的名称或来源选择器为准，不要假设其目录名必然被直接沿用。
- **改动未被发现：** 运行 `holon skills refresh`，随后核对生效列表与其源码路径。
- **Lock 文件不匹配：** 运行 `holon skills check my-skill` 查看报告的差异；仅在确认本地内容是你希望锁定的版本时，才执行 `holon skills reconcile my-skill`。
- **已处于可用状态但未被使用：** 确保所选任务确实匹配该工作流，并显式提示 Agent 查阅该 Skill。独立排查可能缺失的底层工具或环境依赖。

命令名称、来源规范及 `skills.toml` 格式说明请参阅 [Skill 参考](/zh-CN/reference/skills.md)以及[模板清单参考](/zh-CN/reference/agent-templates.md#skillstoml)。
