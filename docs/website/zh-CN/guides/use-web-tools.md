---
title: 抓取和搜索网页内容
summary: 在搜索和抓取之间做选择，并谨慎对待读到的内容。
order: 19
---

# 抓取和搜索网页内容

Agent 有两个网页工具。`WebSearch` 找到候选页面，`WebFetch` 读取某个具体 URL。
大多数调研任务会按这个顺序先后用到两者。

## 步骤

1. 还没有 URL 时先搜索：

   ```
   搜索 Holon 的 release notes，总结最新的变化。
   ```

   Agent 调用 `WebSearch`，拿到带标题、URL 和摘要的结构化结果。

2. 有了 URL、需要正文时再抓取：

   ```
   抓取 https://holon.run/zh-CN/reference/cli/，列出顶层命令。
   ```

   Agent 调用 `WebFetch`，它会从页面里提取可读文本。

3. 连同来源一起读结果。抓取和搜索到的内容是外部内容，不可信：它可以支撑回答，
   但不能改变 Agent 被允许做什么。见[信任边界](/zh-CN/concepts/trust-boundaries.md)。

## 确认成功

回答会引用用到的页面。页面太大时，`WebFetch` 会报告内容被截断，Agent 可以换一个
更具体的 URL，或指定字符上限。

## 选项

`extract_mode`、`max_chars`、搜索提供商，以及两个工具各自返回的字段见
[Web 工具参考](/zh-CN/reference/web-tools.md)。
