---
title: CLI 稳定性策略
summary: Holon 命令行接口与机器可读输出契约的支持策略。
order: 12
---
<!-- maintenance: hand-written policy page; review when CLI stability levels or change policy change. Last reviewed against v0.44.1. -->

# CLI 稳定性策略

Holon 仍处于 1.0 之前，但并非每个 CLI 接口都有相同的变更风险。
本策略说明哪些命令行接口对脚本安全、哪些主要供人类或调试使用，
以及未来的命令变更需要达到什么标准。

配合以下页面阅读：

- [CLI 参考](/zh-CN/reference/cli.md)，了解当前命令树和常见工作流。
- [CLI 契约清单](/zh-CN/reference/cli-contract-inventory.md)，了解逐命令的稳定性级别、输出模式和已知契约缺口。
- [CLI 退出码](/zh-CN/reference/cli-exit-codes.md)，了解进程退出码和 stdout/stderr 路由。
- [API 契约清单](/zh-CN/reference/api-contract-inventory.md)，了解许多 CLI JSON 输出所镜像的 HTTP 响应形态。

## 稳定性级别

| 级别 | 预期用途 | 支持策略 |
|-------|--------------|----------------|
| `stable` | 用户和脚本可以依赖的公开 CLI 契约。 | 避免破坏性变更。若无法避免，保留有记录的迁移路径并在发布说明中说明。 |
| `experimental` | 公开可达、但仍在成型的接口。 | 在 Holon 运行时模型稳定期间可能变化。可行时优先提供警告、别名或兼容输出，再考虑移除。 |
| `internal` | 调试、fixture、本地开发或运行时检查接口。 | 不面向外部自动化。可能随实现细节变化。 |
| `deprecated` | 带有记录替代方案的兼容接口。 | 在满足记录的兼容窗口或移除标准前保持可用。不要对新增自动化使用它。 |

当命令有混合输出模式时，把级别应用到你所消费的具体接口。例如，某个命令路径可能是稳定候选，而其人类可读措辞仍为 experimental。

## 脚本安全接口

脚本应优先使用同时具备以下全部属性的接口：

1. 在 CLI 参考或契约清单中有记录的命令路径和标志集。
2. 机器可读 JSON 输出，最好经由 Holon 规范化的 JSON 打印路径输出，而非直接透传原始 HTTP 响应。
3. 有记录的响应归属方：CLI 契约清单，或当 CLI 镜像控制平面响应时的 HTTP/API 清册。
4. [CLI 退出码契约](/zh-CN/reference/cli-exit-codes.md)中明确的退出码行为。

当前的面向脚本候选包括：

- `holon daemon status`、`holon daemon logs` 以及守护进程生命周期命令。
- `holon config get|set|unset|schema` 以及打印 JSON 的凭据/provider 管理命令。
- `holon agent list` 和 `holon agent status`。
- `holon workspace attach|exit|detach`，前提是 workspace 身份契约稳定。
- `holon run --json` 和 `holon solve --json` 仅限已记录的响应形态；它们的人类输出仍面向操作者。

脚本不应解析确切的帮助文本、tracing 日志、调试措辞或人类摘要。这些输出面向人类，可能为提升清晰度而改变。

## 人类与诊断接口

以下接口有意不作为主要的自动化契约：

- `holon --help` 和 `holon <command> --help` 的措辞。命令名和标志名是契约材料；格式和说明文字不是。
- `holon run` 和 `holon solve` 的默认人类输出。
- `holon serve` 的启动摘要和日志。
- `holon debug *` 命令。
- `stderr`，包括 tracing 日志、Clap 错误、凭据提示以及 provider/运行时诊断。

如果某个诊断接口对自动化变得重要，应把所需字段提升到 JSON 响应或有记录的 API/CLI 契约中，而不是解析调试文本。

## 已废弃的 `holon control`

`holon control` 已废弃。请改用 agent 生命周期命令：

| 已废弃命令 | 替代 |
|--------------------|-------------|
| `holon control start --agent <AGENT>` | `holon agent start <AGENT>` |
| `holon control stop --agent <AGENT>` | `holon agent stop <AGENT>` |
| `holon control abort --agent <AGENT>` | `holon agent abort <AGENT>` |

兼容策略：

- 除非发布说明宣布更窄的移除窗口，否则在 0.x 线内保持 `holon control start|stop|abort` 可达。
- 不要仅为 `holon control` 添加新选项或行为；改进应先落到 `holon agent ...`。
- 移除前，替代命令必须在生命周期状态变更和退出/错误报告上具备等价且有记录的行为。
- 只有在 CLI 契约清单记录移除标准且发布说明已把用户指向替代后，才应执行移除。

## 变更要求

变更 CLI 行为时：

1. 当命令路径、标志或常见工作流变化时，更新 [CLI 参考](/zh-CN/reference/cli.md)。
2. 当稳定性分类、输出模式或脚本安全性变化时，更新 [CLI 契约清单](/zh-CN/reference/cli-contract-inventory.md)。
3. 当 CLI 输出镜像了变化后的控制平面响应时，更新 [API 契约清单](/zh-CN/reference/api-contract-inventory.md)。
4. 为 stable 和 stable 候选命令的形态、输出或退出码行为添加或更新测试。
5. 避免为自动化引入新的原始输出路径。若必须透传原始输出，在规范化前将其记录为 experimental。

稳定的 CLI 接口应当变得乏味：被明确命名、有测试、并在用户写脚本前会查看的地方有记录。
