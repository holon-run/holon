---
title: 参考
summary: Holon CLI、配置和控制平面的当前契约快照。
order: 40
---

# 参考

参考页面描述 Holon 当前公开接口的实际行为——而不是计划或承诺中的行为。
它们基于编译产物（`holon --help`、`holon config schema`）验证，行为变化时应随之刷新。

> **稳定性说明：** 运行时处于 1.0 之前。CLI 形态、配置键和 HTTP 端点可能不经通知即变更。
> 各参考页面在适用处记录了最后一次重新生成时对应的版本。设计方向和稳定性状态见仓库
> [RFC 索引](https://github.com/holon-run/holon/tree/main/docs/rfcs)（英文）。

> 本节除本页外暂为英文，链接会跳转到对应英文页面。中文翻译在逐步补充中。

## 本节页面

- [CLI 参考](/reference/cli.md)（英文）
  Holon 命令行界面——基于 holon --help 验证（v0.30.0）。

- [CLI 契约清单](/reference/cli-contract-inventory.md)（英文）
  Holon 命令行参数、输出和后续契约工作的第一版稳定性清单。

- [CLI 稳定性策略](/reference/cli-stability-policy.md)（英文）
  Holon 命令行接口和机器可读输出契约的支持策略。

- [CLI 退出码](/reference/cli-exit-codes.md)（英文）
  Holon 命令行界面的退出码和流路由契约。

- [配置](/reference/configuration.md)（英文）
  Holon 配置文件、配置键、凭据、环境变量和诊断。

- [HTTP 控制平面](/reference/http-control-plane.md)（英文）
  如何理解 Holon 的无头集成接口。

- [OpenAPI schema](/reference/openapi.json)（英文）
  Holon 当前 HTTP 控制平面接口的基线 OpenAPI 3.1 schema。

- [API 契约清单](/reference/api-contract-inventory.md)（英文）
  Holon HTTP 控制平面 API 参数、响应和第二阶段契约工作的基线后稳定性清单。

- [模型工具 schema 清单](/reference/model-tool-schema-inventory.md)（英文）
  Holon 面向模型的内置工具 schema、结果信封和稳定性标签的版本化清单。

- [运行时状态枚举清单](/reference/runtime-status-enum-inventory.md)（英文）
  稳定序列化运行时生命周期和状态枚举的机器可读基线。

- [支持的模型](/reference/models.md)（英文）
  Holon 内置支持的所有模型和提供商的完整参考。

<!-- INDEX:START -->

<!-- INDEX:END -->
