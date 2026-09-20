---
title: 指南
summary: 面向使用、运维和集成 Holon 的任务式操作指南。
order: 30
---

# 指南

每篇指南只回答一个问题：*这件事我要怎么做完？* 它们是从头到尾的操作步骤，
紧贴任务本身。精确的命令、参数、字段和端点放在[参考](/zh-CN/reference/)，
背后的心智模型放在[概念](/zh-CN/concepts/)。

下面的列表按你手头的任务分组：先拿到第一个结果、运行需要等待的工作、
协调多个 Agent、用代码或浏览器驱动 Holon、处理外部内容，以及任务卡住时
如何排查。

<!-- INDEX:START -->

- [运行你的第一个 Holon 任务](./quick-examples.md)
  启动 Holon，完整跑通一个任务并确认结果。
  <!-- mdorigin:index kind=article -->

- [使用 holon solve 执行 GitHub 任务](./run-github-task.md)
  从 issue 或 PR 输入到任务完成，再检查结果。
  <!-- mdorigin:index kind=article -->

- [运行长生命周期任务](./run-long-lived-task.md)
  启动需要等待的工作，查看进度，断线后恢复，并取得最终交付。
  <!-- mdorigin:index kind=article -->

- [委派工作给另一个 Agent](./delegate-work.md)
  把边界清楚的任务交给子 Agent，等待结果并处理返回内容。
  <!-- mdorigin:index kind=article -->

- [通过 HTTP 自动化 Holon](./automate-over-http.md)
  从代码里完成认证、提交工作、跟踪状态并读取结果。
  <!-- mdorigin:index kind=article -->

- [连接远程 Holon 运行时](./connect-remote-runtime.md)
  连接另一台机器上的运行时并验证连接可用。
  <!-- mdorigin:index kind=article -->

- [使用 Web GUI](./use-web-gui.md)
  在浏览器里驱动 Agent、Work Item 和 Skill。
  <!-- mdorigin:index kind=article -->

- [使用模板创建 Agent](./create-agent.md)
  选择模板、创建 Agent，并确认它能接下一个任务。
  <!-- mdorigin:index kind=article -->

- [为 Agent 添加 Skill](./use-skills.md)
  找到 skill、安装到库、为某个 Agent 启用，并确认它已生效。
  <!-- mdorigin:index kind=article -->

- [抓取和搜索网页内容](./use-web-tools.md)
  在搜索和抓取之间做选择，并谨慎对待读到的内容。
  <!-- mdorigin:index kind=article -->

- [生成图像](./generate-image.md)
  写好提示词、生成图片，并找到落盘的结果。
  <!-- mdorigin:index kind=article -->

- [用视觉工具观察图像](./inspect-image.md)
  让视觉模型看一张本地图片，并读回它看到的内容。
  <!-- mdorigin:index kind=article -->

- [排查 Holon 任务问题](./troubleshooting.md)
  把卡住、失败或无输出的任务定位到一个明确的下一步。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
