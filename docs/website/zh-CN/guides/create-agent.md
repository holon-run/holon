---
title: 使用模板创建 Agent
summary: 选择模板、创建 Agent，并确认它能接下一个任务。
order: 17
---

# 使用模板创建 Agent

模板给新 Agent 一个起始角色，需要时还会带上 skill。Agent 只需要创建一次，之后用
ID 找它。

## 前置条件

- daemon 在运行：`holon daemon start`。
- 你知道想要什么角色。模板目录见
  [Agent 模板参考](/zh-CN/reference/agent-templates.md)。

## 步骤

1. 用模板创建 Agent：

   ```bash
   holon agent create reviewer --template code-reviewer
   ```

   不带 `--template` 时，Agent 只有一份通用的默认契约。只要这个 Agent 有明确
   职责，就用模板。

2. 确认它已经存在并且就绪：

   ```bash
   holon agent list
   holon agent status reviewer
   ```

3. 交给它一个任务，看结果：

   ```bash
   holon run --agent reviewer "Review the open PR on holon-run/holon#1234"
   ```

   需要活得比命令更久的工作，就从 TUI 里启动：`holon tui`。

## 确认成功

`holon agent list` 里能看到这个 Agent，`holon agent status` 显示它处于唤醒状态，
你的任务最后以结果简报或一个向你提问结束。

## 内置目录不够用的时候

用你自己维护的模板创建：

```bash
holon agent create my-agent --template /path/to/my-template
holon agent create my-agent --template https://github.com/owner/repo/tree/main/templates/my-template
```

模板结构、选择规则，以及 `template.toml`、`skills.toml` 的 schema 见
[Agent 模板参考](/zh-CN/reference/agent-templates.md)。
