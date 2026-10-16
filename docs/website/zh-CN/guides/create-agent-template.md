---
title: 创建 Agent 模板
summary: 编写角色契约，添加可选的 Skill 依赖，并通过新 Agent 验证其有效性。
order: 30
---

# 创建 Agent 模板

当需要让多个新 Agent 拥有相同的初始角色定位时，可以创建模板。后续修改模板不会影响已有 Agent 自身的指引文件。心智模型请参阅 [Agent 模板概念](/zh-CN/concepts/agent-templates.md)。

## 前置条件

- 正常运行的守护进程与可用的模型连接。
- 一个新建的模板目录以及一个未被使用的测试 Agent ID。
- 对计划引入的所有 Skill 来源具备读取权限。

## 操作步骤

1. 创建独立的模板目录（避免覆盖现有模板）：

   ```bash
   mkdir -p "$HOME/holon-template-examples"
   mkdir "$HOME/holon-template-examples/docs-reader"
   ```

2. 在 `docs-reader/` 目录下创建 `AGENTS.md`：

   ```markdown
   # 文档审阅员

   阅读操作者提供的文档并指出表述模糊的步骤。
   说明具体位置、歧义所在以及建议的修改措辞。
   默认以只读方式运行；未经明确授权不要修改源文件。
   ```

3. （可选）声明依赖的 Skill：
   在 `docs-reader/skills.toml` 中声明初始化时需要安装的 Skill：

   ```toml
   [[skills]]
   source = "writing-clearly-and-concisely"
   ```

   确保该 Skill 已经存在于本地技能库或系统可解析的路径中。

4. 验证模板并初始化测试 Agent：

   ```bash
   holon agent create test-reader --template "$HOME/holon-template-examples/docs-reader"
   holon agent get test-reader --detail full
   ```

5. 发送测试任务核验证据：

   ```bash
   holon run --agent test-reader "审阅 README.md 的开头段落并给出建议"
   ```

   检查输出是否遵循了模板规定的角色定位与只读限制。测试完成后清理临时 Agent：

   ```bash
   holon agent delete test-reader --yes
   ```

## 另见

- [Agent 模板概念](/zh-CN/concepts/agent-templates.md)
- [Agent 模板参考](/zh-CN/reference/agent-templates.md)
- [定制 Agent 角色与指引](customize-agent.md)
