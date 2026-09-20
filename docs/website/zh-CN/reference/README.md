---
title: 参考
summary: Holon CLI、配置和控制平面的当前契约快照。
order: 40
---

# 参考

参考页面描述 Holon 当前公开接口的实际行为——而不是计划或承诺中的行为。
有的对照编译后的运行时（`holon --help`、`holon config schema`）核对，有的描述某个
工具或功能接口并标出来源。行为变化时应随之刷新。

> **稳定性说明：** 运行时处于 1.0 之前。CLI 形态、配置键和 HTTP 端点可能不经通知即变更。
> 各参考页面在适用处记录了最后一次重新生成时对应的版本。设计方向和稳定性状态见仓库
> [RFC 索引](https://github.com/holon-run/holon/tree/main/docs/rfcs)（英文）。

> 本节页面均已译为中文。数据快照（`openapi.json`、`*-inventory.json`）和由源码生成的
> `models.md` 正文保持英文，随英文版同步。

机器可读接口：[OpenAPI 3.1 schema](/reference/openapi.json) 描述当前 HTTP 控制平面。

<!-- INDEX:START -->

- [CLI 参考](./cli.md)
  Holon 命令行界面——基于 holon --help 验证（v0.44.1）。
  <!-- mdorigin:index kind=article -->

- [CLI 契约清单](./cli-contract-inventory.md)
  Holon 命令行参数、输出和后续契约工作的第一版稳定性清单。
  <!-- mdorigin:index kind=article -->

- [CLI 稳定性策略](./cli-stability-policy.md)
  Holon 命令行接口与机器可读输出契约的支持策略。
  <!-- mdorigin:index kind=article -->

- [CLI 退出码](./cli-exit-codes.md)
  Holon 命令行界面的退出码与流路由契约。
  <!-- mdorigin:index kind=article -->

- [配置](./configuration.md)
  Holon 的配置文件、配置键、凭据、环境变量与诊断。
  <!-- mdorigin:index kind=article -->

- [HTTP 控制平面](./http-control-plane.md)
  如何理解 Holon 的无头集成接口。
  <!-- mdorigin:index kind=article -->

- [API 契约清单](./api-contract-inventory.md)
  Holon HTTP 控制平面 API 参数、响应和第二阶段契约工作的基线后稳定性清单。
  <!-- mdorigin:index kind=article -->

- [模型工具 schema 清单](./model-tool-schema-inventory.md)
  Holon 面向模型的内置工具 schema、结果信封和稳定性标签的版本化清单。
  <!-- mdorigin:index kind=article -->

- [运行时状态枚举清单](./runtime-status-enum-inventory.md)
  稳定序列化运行时生命周期和状态枚举的机器可读基线。
  <!-- mdorigin:index kind=article -->

- [Workspace 与执行环境](./workspaces.md)
  workspace、执行根和 worktree 的绑定、切换与隔离契约。
  <!-- mdorigin:index kind=article -->

- [Agent 模板](./agent-templates.md)
  模板目录、选择规则，以及初始化 Agent 所用的 AGENTS.md、template.toml 和 skills.toml schema。
  <!-- mdorigin:index kind=article -->

- [Skills](./skills.md)
  Skill 来源、skills.toml schema，以及安装、启用、更新和校验 skill 的 CLI 命令。
  <!-- mdorigin:index kind=article -->

- [TUI](./tui.md)
  终端 UI 参考：斜杠命令、快捷键、面板和连接控制。
  <!-- mdorigin:index kind=article -->

- [工作项](./work-items.md)
  工作项字段、状态、生命周期操作，以及操作它们的 CLI 和 HTTP 接口。
  <!-- mdorigin:index kind=article -->

- [可观测性](./observability.md)
  Trace 导出、受保护的指标端点，以及 OTLP、OpenMetrics、仪表盘和告警的配置。
  <!-- mdorigin:index kind=article -->

- [Web 工具](./web-tools.md)
  WebFetch 和 WebSearch 的参数、提取模式、搜索提供商、截断和来源处理。
  <!-- mdorigin:index kind=article -->

- [图像观察](./view-image.md)
  图像观察工具的输入、视觉模型选择、响应元数据和兼容性限制。
  <!-- mdorigin:index kind=article -->

- [图像生成](./image-generation.md)
  图像生成工具的参数、支持的模型、输出尺寸和格式，以及错误行为。
  <!-- mdorigin:index kind=article -->

- [支持的模型](./models.md)
  Holon 内置支持的所有模型和提供商的完整参考。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
