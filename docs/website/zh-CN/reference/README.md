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

> 本节页面均已译为中文。数据快照（`openapi.json`、`*-inventory.json`）和由源码生成的
> `models.md` 正文保持英文，随英文版同步。

机器可读接口：[OpenAPI 3.1 schema](/reference/openapi.json) 描述当前 HTTP 控制平面。

<!-- INDEX:START -->

- [CLI 参考](./cli.md)
  Holon 命令行界面——基于 holon --help 验证（v0.39.0）。
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

- [支持的模型](./models.md)
  Holon 内置支持的所有模型和提供商的完整参考。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
