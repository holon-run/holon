---
title: 参考
summary: Holon CLI、配置和控制平面的当前契约快照。
order: 40
---

# 参考

参考页面描述 Holon 当前公开接口的实际行为——而不是计划或承诺中的行为。
它们基于编译产物（`holon --help`、`holon config schema`、路由清单）核对验证，是语法、参数与接口的权威事实来源。

## 涵盖内容

- **命令行接口 (CLI)：** 完整命令树、选项参数与状态退出码。
- **配置项规范：** 配置文件格式、配置键校验与环境变量映射。
- **HTTP 控制面：** RESTful API 端点、认证令牌机制与请求响应 Schema。
- **模型编目与内置工具：** 33 家 Provider 模型支持列表与内置工具契约。

分步任务操作教程请查阅 [指南](/zh-CN/guides/)；内部调度器与状态机契约请查阅 [运行时规格](/zh-CN/spec/)。

> **稳定性说明：** 运行时处于 1.0 之前。CLI 形态、配置键和 HTTP 端点可能不经通知即变更。
> 各参考页面在适用处记录了最后一次重新生成时对应的版本。设计方向和稳定性状态见仓库
> [RFC 索引](https://github.com/holon-run/holon/tree/main/docs/rfcs)（英文）。

> 本节页面均已译为中文。数据快照（`openapi.json`、`*-inventory.json`）和由源码生成的
> `models.md` 正文保持英文，随英文版同步。

## 手写页面与生成基线

手写页面基于某个运行时产物核对，并记录最后一次核对的版本。生成页面和机器可读基线
由源码刷新，不在此手工编辑。

| 页面 | 事实来源 | 刷新方式 |
|---|---|---|
| `cli.md` | `holon --help` | 命令树变化时重新生成 |
| `configuration.md` | `holon config schema`、`holon config list` | 配置键变化时重新核对 |
| `http-control-plane.md` | Axum 路由树、OpenAPI 3.1 schema（`openapi.json`） | 路由或载荷变化时重新核对 |
| `models.md` | 由 `src/model_catalog.rs` 生成 | `cargo run --bin holon-docgen -- models > docs/website/reference/models.md`，再执行 `python3 docs/website/.tools/sync-generated-pages.py` |
| `*-inventory.md`、`*-inventory.json` | 生成的 JSON 基线 | `make snapshots-refresh`，然后审查差异 |

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

- [支持的模型](./models.md)
  Holon 内置支持的所有模型和提供商的完整参考。
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
