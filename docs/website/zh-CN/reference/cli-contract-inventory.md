---
title: CLI 契约清单
summary: Holon 命令行参数、输出和后续契约工作的第一版稳定性清单。
order: 11
---

# CLI 契约清单

本清单按 `src/main.rs` 记录 Holon 当前的 CLI 接口，并对照 `target/debug/holon --help`（`holon 0.39.0`）核验。
它是稳定性规划文档，不代表清单中的每条命令都已稳定。

当前实现把 CLI 收在一个 Rust 二进制中：

- `src/main.rs` 定义 Clap 命令树和大部分 CLI 处理器。
- `docs/website/reference/cli.md` 是面向用户的命令参考。
- `docs/website/reference/cli-stability-policy.md` 记录 stable、experimental、internal 和
  deprecated CLI 接口的面向用户支持策略。
- `docs/website/reference/configuration.md` 记录配置文件、凭据相关环境变量和诊断。

## 稳定性级别

这些标签背后的面向用户支持策略见 [CLI 稳定性策略](/zh-CN/reference/cli-stability-policy.md)。

| 级别 | 含义 | 变更策略 |
|---|---|---|
| `stable` | 面向公众的预期接口，用户和脚本可以合理依赖。 | 避免破坏性变更；需要发布说明和迁移路径。 |
| `experimental` | 公开可达，但仍在成型。 | 1.0 前可能变化；移除前优先提供警告或别名。 |
| `internal` | 调试、运行时或本地开发接口。 | 不面向外部自动化；内部实现变化时可能改变。 |
| `deprecated` | 为兼容而保留，但已被其他接口取代。 | 记录替代方案，仅通过明确的废弃计划移除。 |

## 跨领域 CLI 契约

| 接口 | 当前行为 | 初始稳定性 | 备注 |
|---|---|---:|---|
| 命令解析器 | 由 Clap 派生的命令树，根级带 `--help` 和 `--version`。 | `stable` | 命令名和标志名是价值最高的 CLI 契约。 |
| 帮助文本 | 人类可读的 Clap 输出。 | `experimental` | 对用户有用；不应把确切的间距和措辞当作机器可读。 |
| 错误 | 由二进制运行时渲染的 Clap 校验错误或 `anyhow` 错误。 | `experimental` | 在宣布稳定前，退出码形态需要明确的测试。 |
| JSON 输出 | 面向脚本的 JSON 命令通过共享的 `print_json` 路径向 stdout 打印美化 JSON。 | `experimental` | JSON 字段形态通常来自运行时/控制平面结构体，需要与 API 清册对齐。 |
| 人类输出 | `run`、`solve`、`serve`、`debug latency`、`debug prompt` 以及部分 debug/export 命令向 stdout 输出人类文本。 | `experimental` | 除非命令有意面向脚本，否则不要对完整措辞做快照。 |
| stderr | tracing 日志、Clap 错误、凭据提示以及部分 provider/运行时诊断。 | `experimental` | 凭据提示有意写到 stderr。 |
| stdin | 目前只有 `config credentials set --stdin` 从 stdin 读取。 | `stable` 候选 | 交互细节需要专门的测试。 |
| 配置/环境变量 | 大多数命令加载 `AppConfig`；配置命令用 `$HOLON_HOME` 定位离线配置路径。 | `experimental` | 更广的环境变量范围见配置参考。 |

## 命令清单

### 根命令

| 命令 | 参数 | 选项 | 输出 | 初始稳定性 | 备注 |
|---|---|---|---|---:|---|
| `holon --help` | none | `-h, --help`, `-V, --version` | 向 stdout 输出人类帮助 | `stable` 候选 | 应由命令树快照测试覆盖。 |
| `holon <COMMAND> --help` | 取决于命令 | 取决于命令 | 向 stdout 输出人类帮助 | 形态为 `stable` 候选；措辞为 `experimental` | 命令形态变化时重新生成 `cli.md`。 |

### 服务器与守护进程

| 命令 | 参数 | 选项 | 输出 | 初始稳定性 | 备注 |
|---|---|---|---|---:|---|
| `holon serve` | none | `--access <local\|tunnel\|lan\|tailnet>` 默认 `local`；`--host <HOST>`；`--listen <LISTEN>`；`--port <PORT>`；`--advertise <ADVERTISE>`；`--token <TOKEN>`；`--token-file <TOKEN_FILE>` | 长期运行的服务器；启动摘要在 stdout；日志/tracing 在 stderr | `experimental` | 非 loopback/tailnet/lan 访问需要通过标志、文件或 `HOLON_CONTROL_TOKEN` 提供控制令牌。 |
| `holon daemon start` | none | 与 `serve` 相同的 `ServeOptions` | JSON 守护进程生命周期响应 | `stable` 候选 | 内联令牌通过环境变量传给子进程，而非 argv。 |
| `holon daemon stop` | none | none | JSON 守护进程生命周期响应 | `stable` 候选 | 使用本地守护进程生命周期辅助函数。 |
| `holon daemon status` | none | none | JSON 守护进程状态响应 | `stable` 候选 | 重要的本地检查接口。 |
| `holon daemon restart` | none | 与 `serve` 相同的 `ServeOptions` | JSON 守护进程生命周期响应 | `stable` 候选 | 与 `serve` 相同的访问/令牌校验。 |
| `holon daemon logs` | none | `--tail <TAIL>` 默认 `80` | JSON 守护进程日志响应 | `stable` 候选 | `daemon logs` 被记录为本地故障排查接口。 |

### 离线配置

这些命令直接操作持久化配置或凭据文件，不要求守护进程运行。

| 命令 | 参数 | 选项 | 输出 | 初始稳定性 | 备注 |
|---|---|---|---|---:|---|
| `holon config get` | `<KEY>` | none | 该键的 JSON 值 | `stable` 候选 | 可达时优先使用守护进程运行时配置 API，保持 stdout 形态；键集来自配置契约。 |
| `holon config set` | `<KEY> <VALUE>` | none | 写入后的 JSON 值 | `stable` 候选 | 可达时优先使用守护进程运行时配置 API，否则回退到离线模式，并在 stderr 报告 `applied_via`；守护进程的拒绝会透出原因。 |
| `holon config unset` | `<KEY>` | none | JSON `{ "key": ..., "status": "unset" }` | `stable` 候选 | 可达时优先使用守护进程运行时配置 API，否则回退到离线模式，并在 stderr 报告 `applied_via`；若脚本依赖该状态字符串，应将其锁定。 |
| `holon config list` | none | none | 完整的持久化配置 JSON | `experimental` | 可达时优先使用守护进程运行时配置 API，保持 stdout 形态；会暴露较广的配置文件形态。 |
| `holon config schema` | none | none | JSON 配置 schema/元数据 | `stable` JSON | 由 `tests/cli_json_contract.rs` 锁定；条目对象暴露 `key`、`kind`、`description`、`default` 以及可选的 `allowed_values`。 |
| `holon config doctor` | none | none | JSON provider/系统诊断 | `experimental` | 诊断形态可能随 provider 变化。 |
| `holon config models list` | none | none | JSON 模型可用性列表 | `experimental` | Provider 目录和可用性细节仍在演进。 |

### Provider 配置

| 命令 | 参数 | 选项 | 输出 | 初始稳定性 | 备注 |
|---|---|---|---|---:|---|
| `holon config providers set` | `<PROVIDER>` | `--transport <TRANSPORT>`；`--base-url <BASE_URL>`；`--credential-source <SOURCE>` 默认 `none`；`--credential-kind <KIND>` 默认 `none`；`--credential-env <ENV>`；`--credential-profile <PROFILE>`；`--credential-external <COMMAND>` | JSON `{ "applied_via": "offline_store", "provider": ... }` | 命令形态为 `stable` 候选；provider 对象为 `experimental` | 内置 provider 可能拒绝不兼容的 transport 覆盖。 |
| `holon config providers get` | `<PROVIDER>` | none | JSON provider 视图 | `experimental` | 输出使用运行时 provider 视图。 |
| `holon config providers list` | none | none | provider 视图的 JSON 数组/对象 | `experimental` | 输出形态应与 API/配置清册对齐。 |
| `holon config providers remove` | `<PROVIDER>` | none | JSON `{ "applied_via": "offline_store", "provider": ..., "status": "removed\|not_configured" }` | `stable` JSON | 由 `tests/cli_json_contract.rs` 锁定；状态字符串面向脚本。 |
| `holon config providers doctor` | `<PROVIDER>` | none | JSON provider 视图加模型链诊断 | `experimental` | 诊断细节可能变化。 |

### 凭据配置

| 命令 | 参数 | 选项 | 输出 | 初始稳定性 | 备注 |
|---|---|---|---|---:|---|
| `holon config credentials set` | `<PROFILE>` | 必填 `--kind <KIND>`；`--stdin` 或 `--material <MATERIAL>` 二选一 | JSON `{ "applied_via": "offline_store", "credential": { "profile": ..., "kind": ..., "configured": true } }` | `stable` JSON | 由 `tests/cli_json_contract.rs` 锁定；`--stdin` 提示写到 stderr；raw `--material` 有意不推荐用于机密。 |
| `holon config credentials list` | none | none | 带 `profile`、`kind` 和 `configured` 的 JSON 凭据 profile 列表；绝不包含凭据材料 | `stable` JSON | 由 `tests/cli_json_contract.rs` 锁定；绝不能暴露凭据材料。 |
| `holon config credentials remove` | `<PROFILE>` | none | JSON `{ "applied_via": "offline_store", "credential": { "profile": ..., "kind": ..., "configured": false } }` | `stable` JSON | 由 `tests/cli_json_contract.rs` 锁定；不存在的 profile 返回 `kind: "unknown"` 和 `configured: false`。 |

### Agent 交互与检查

除非另有说明，这些命令要求本地控制平面可达。

| 命令 | 参数 | 选项 | 输出 | 初始稳定性 | 备注 |
|---|---|---|---|---:|---|
| `holon prompt` | `<TEXT>` | `--agent <AGENT>` | JSON 控制平面 prompt 响应 | `experimental` | 轻量 prompt 路径；响应形态属于控制平面 API 清册。 |
| `holon tail` | none | `--limit <LIMIT>` 默认 `20`；`--agent <AGENT>` | JSON 近期 brief/日志尾部 | `stable` 候选 | 结果形态应与 brief/输出契约对齐。 |
| `holon transcript` | none | `--limit <LIMIT>` 默认 `50`；`--agent <AGENT>` | JSON transcript 条目 | `stable` 候选 | transcript 条目的稳定性需要 API 清册。 |
| `holon task run` | `<SUMMARY>` | 必填 `--cmd <CMD>`；`--workdir <WORKDIR>`；`--shell <SHELL>`；`--login <true\|false>`；`--tty`；`--yield-time-ms <MS>`；`--max-output-tokens <N>`；`--agent <AGENT>` | 美化 JSON 控制平面响应 | `experimental` | 通过控制平面创建命令任务。 |
| `holon task status` | `<TASK_ID>` | `--agent <AGENT>` | 美化 JSON `TaskStatusSnapshot` | `experimental` | 通过任务状态 API 读取任务生命周期状态。 |
| `holon task output` | `<TASK_ID>` | `--block`；`--timeout-ms <MS>`；`--agent <AGENT>` | 美化 JSON `TaskOutputResult` | `experimental` | 输出预览长度遵循任务创建时的 `--max-output-tokens`；本命令只控制就绪等待。 |
| `holon task input` | `<TASK_ID>` | 必填 `--text <TEXT>`；`--agent <AGENT>` | 美化 JSON `TaskInputResult` | `experimental` | 向命令任务 stdin/TTY 或受监督子 Agent 的后续输入发送可信操作者文本。 |
| `holon task stop` | `<TASK_ID>` | `--agent <AGENT>` | 美化 JSON `TaskStopResult` | `experimental` | 通过控制平面请求取消受管任务。 |
| `holon work-item list` | none | `--limit <LIMIT>` 默认 `50`；`--agent <AGENT>` | `WorkItemRecord` 的美化 JSON 数组 | `experimental` | JSON schema 归属为 HTTP/API `WorkItemRecord` 读模型，由 `/agents/:agent_id/work-items` 返回。 |
| `holon work-item get` | `<WORK_ITEM_ID>` | `--agent <AGENT>` | 美化 JSON `WorkItemRecord` | `experimental` | 通过 `/agents/:agent_id/work-items/:work_item_id` 读取单个工作项；`create`、`pick`、`update`、`complete` 子命令已存在，但其变更 API 契约仍在稳定中。 |
| `holon timer` | none | 旧式创建语法：必填 `--after-ms <MS>`；`--every-ms <MS>`；`--summary <SUMMARY>`；`--agent <AGENT>` | 美化 JSON `TimerRecord` | `experimental` | `holon timer create` 的向后兼容别名。 |
| `holon timer create` | none | 必填 `--after-ms <MS>`；`--every-ms <MS>`；`--summary <SUMMARY>`；`--agent <AGENT>` | 美化 JSON `TimerRecord` | `experimental` | 通过控制平面创建一次性或重复定时器。 |
| `holon timer list` | none | `--limit <LIMIT>` 默认 `50`；`--agent <AGENT>` | `TimerRecord` 的美化 JSON 数组 | `experimental` | 通过 agent 定时器 API 读取近期定时器。 |
| `holon timer cancel` | `<TIMER_ID>` | `--agent <AGENT>` | 美化 JSON `TimerRecord` | `experimental` | 取消活跃定时器；已取消的定时器操作幂等。 |

### Agent 生命周期与模型选择

| 命令 | 参数 | 选项 | 输出 | 初始稳定性 | 备注 |
|---|---|---|---|---:|---|
| `holon agent` / `holon agents` | 可选子命令 | none | 默认为 `agent list` JSON | `stable` 候选 | `agents` 是别名。 |
| `holon agent list` | none | none | JSON agent 条目 | `stable` 候选 | 公开的多 Agent 检查接口。 |
| `holon agent status` | 可选 `[AGENT_ID]` | none | JSON agent 状态 | `stable` 候选 | 位置参数 agent id；默认为配置的默认 agent。 |
| `holon agent create` | `<AGENT_ID>` | `--template <TEMPLATE>` | 美化 JSON 控制平面响应 | `stable` 候选 | 模板标识符契约应与 Agent 初始化文档对齐。 |
| `holon agent repair` | `<AGENT_ID>` | none | 美化 JSON `AgentDetail` | `experimental` | 重试未完成的创建后引导步骤，而不重建 Agent。 |
| `holon agent start` | 可选 `[AGENT_ID]` | none | JSON 生命周期控制响应 | `stable` 候选 | 已废弃 `control start` 的替代。 |
| `holon agent stop` | 可选 `[AGENT_ID]` | none | JSON 生命周期控制响应 | `stable` 候选 | 已废弃 `control stop` 的替代。 |
| `holon agent abort` | 可选 `[AGENT_ID]` | none | 美化 JSON 控制平面响应 | `stable` 候选 | 已废弃 `control abort` 的替代；与 start/stop 共用生命周期 JSON 输出路径。 |
| `holon agent model get` | 可选 `[AGENT_ID]` | none | JSON 模型覆盖/状态片段 | `stable` 候选 | 从 agent 状态读取 `summary.model`。 |
| `holon agent model set` | `<MODEL> [AGENT_ID]` | none | JSON 模型覆盖响应 | `stable` 候选 | 位置参数 `AGENT_ID` 已测试。 |
| `holon agent model clear` | 可选 `[AGENT_ID]` | none | JSON 模型覆盖响应 | `stable` 候选 | 应与 set/get 共享契约。 |
| `holon control` | `<start\|stop\|abort>` | `--agent <AGENT>` | 美化 JSON 生命周期响应 | `deprecated` | 使用 `holon agent start|stop|abort [agent-id]`。 |

已废弃的 `holon control` 兼容性记录在
[CLI 稳定性策略](/zh-CN/reference/cli-stability-policy.md)。
新自动化应使用 `holon agent ...` 生命周期命令。

### Skills

技能管理分为库操作和 Agent 启用两部分：

| 命令 | 参数 | 选项 | 输出 | 初始稳定性 | 备注 |
|---|---|---|---|---:|---|
| `holon skills catalog` | none | none | JSON catalog 响应 | `experimental` | 列出本地 Skill Library 中的所有技能。 |
| `holon skills refresh` | none | none | JSON catalog 响应 | `experimental` | 重新扫描本地技能根目录以刷新运行时 catalog。不与锁文件对账，也不拉取远程更新。 |
| `holon skills add` | `<SOURCE>` | `--remote`；`--skill <SKILL>`；`--copy` | 美化 JSON 控制平面响应 | `experimental` | 向本地 Skill Library 添加技能。本地路径为目录时相对 cwd 解析。 |
| `holon skills remove` | `<NAME>` | none | 美化 JSON 控制平面响应 | `experimental` | 从本地 Skill Library 移除技能。 |
| `holon skills check` | `[NAME]` | none | 美化 JSON 控制平面响应 | `experimental` | 对照 `.skill-lock.json` 检查 Skill Library 一致性。 |
| `holon skills reconcile` | `[NAME]` | none | 美化 JSON 控制平面响应 | `experimental` | 将库条目与锁文件对账。 |
| `holon skills list` | none | `--agent <AGENT>` | JSON agent 技能响应 | `experimental` | 列出某 agent 已启用/生效的技能。 |
| `holon skills enable` | `<NAME>` | `--agent <AGENT>`；`--copy` | 美化 JSON 控制平面响应 | `experimental` | 为 agent 启用本地已知技能。 |
| `holon skills disable` | `<NAME>` | `--agent <AGENT>` | 美化 JSON 控制平面响应 | `experimental` | 为 agent 禁用技能。 |
| `holon skills install` | `<NAME_OR_PATH>` | `--remote`；`--skill <SKILL>`；`--copy`；`--agent <AGENT>` | 美化 JSON 控制平面响应 | `deprecated` | 兼容别名。库操作用 `skills add`，agent 用 `skills enable`。 |
| `holon skills uninstall` | `<NAME>` | `--agent <AGENT>` | 美化 JSON 控制平面响应 | `deprecated` | 兼容别名。库操作用 `skills remove`，agent 用 `skills disable`。 |

### 一次性与 solve 工作流

| 命令 | 参数 | 选项 | 输出 | 初始稳定性 | 备注 |
|---|---|---|---|---:|---|
| `holon run` | `<TEXT>` | `--authority-class <AUTHORITY_CLASS>` 默认 `operator-instruction`；`--json`；`--agent <AGENT>`；`--create-agent`；`--template <TEMPLATE>`；`--max-turns <N>`；`--no-wait-for-tasks`；`--home <HOME>`；`--workspace-root <PATH>`；`--cwd <PATH>` | 默认人类 `render_text()`；带 `--json` 时为美化 JSON | 命令形态为 `stable` 候选；输出为 `experimental` | 核心用户入口。JSON 响应形态应在给出稳定自动化指引前锁定。 |
| `holon solve` | `<REF>` | `--repo <REPO>`；`--base <BASE>`；`--goal <GOAL>`；`--role <ROLE>`；`--agent <AGENT>`；`--template <TEMPLATE>`；`--model <MODEL>`；`--max-turns <N>`；`--authority-class <AUTHORITY_CLASS>` 默认 `operator-instruction`；`--json`；`--home <HOME>`；`--workspace <PATH>`；`--workspace-root <PATH>`；`--cwd <PATH>`；`--input <INPUT>`；`--output <OUTPUT>` | 默认人类 `render_text()`；带 `--json` 时为美化 JSON | `experimental` | GitHub/任务工作流接口。`--workspace` 与 `--workspace-root` 目前被合并。 |

### Workspace

| 命令 | 参数 | 选项 | 输出 | 初始稳定性 | 备注 |
|---|---|---|---|---:|---|
| `holon workspace attach` | `<PATH>` | `--agent <AGENT>` | JSON attach 响应 | `stable` 候选 | Workspace 身份/投影契约是运行时稳定性的核心。 |
| `holon workspace exit` | none | `--agent <AGENT>` | JSON exit 响应 | `stable` 候选 | 应与 workspace 绑定 RFC 对齐。 |
| `holon workspace detach` | `<WORKSPACE_ID>` | `--agent <AGENT>` | JSON detach 响应 | `stable` 候选 | `WORKSPACE_ID` 的稳定性属于 API/运行时清册。 |

### TUI

| 命令 | 参数 | 选项 | 输出 | 初始稳定性 | 备注 |
|---|---|---|---|---:|---|
| `holon tui` | none | `--no-alt-screen`；`--connect <URL>`；`--token <TOKEN>`；`--token-file <PATH>`；`--token-profile <PROFILE>` | 交互式终端 UI | `experimental` | `--connect` 要求且仅要求一个令牌来源。TUI 不是主要的稳定运行时契约。 |

### 调试工具

| 命令 | 参数 | 选项 | 输出 | 初始稳定性 | 备注 |
|---|---|---|---|---:|---|
| `holon debug prompt` | `<TEXT>` | `--agent <AGENT>`；`--authority-class <AUTHORITY_CLASS>` 默认 `operator-instruction` | 人类 prompt dump | `internal` | 仅用于调试的 prompt 检查。 |
| `holon debug latency` | none | `--agent <AGENT>`；`--limit <LIMIT>` 默认 `10`；`--events-limit <EVENTS_LIMIT>` 默认 `5000` | 人类延迟报告 | `internal` | 有用的诊断；措辞不应作为机器契约。 |
| `holon debug scheduler-fixture` | none | `--agent <AGENT>`；必填 `--output <OUTPUT>` | 写出 JSON/JSONL fixture 文件；打印导出摘要 | `internal` | Fixture 文件形态对测试可能有用，但若稳定化应单独记录。 |
| `holon debug scheduler-recovery` | none | `--agent <AGENT>`；`--json`；`--apply`；`--no-backup`（要求 `--apply`） | 只读的规范化恢复诊断；可选的类型化 apply 结果 | `internal` | 默认只读。它是当前二进制中唯一允许打开紧邻的上一版 scheduler schema 而不迁移的命令，因此被阻塞的清理迁移仍可恢复。`--json` 输出 `{ "report": ..., "apply": null | { "changed": ..., "backup_path": ..., "backup_policy": ..., "backup_created": ... } }`。`--apply` 要求守护进程已停止，并默认创建经过校验的 SQLite 备份。显式 `--no-backup` 仅跳过该备份；类型化 source fence、恢复命令和审计证据仍然必需。 |

## CLI 触及的环境与配置输入

这不是完整的配置清册；它只列出在调用命令时明显影响 CLI 行为的环境变量。

| 输入 | 使用方 | 当前行为 | 初始稳定性 |
|---|---|---|---:|
| `HOLON_HOME` | 配置/凭据与运行时配置加载 | 选择 Holon home/配置/凭据路径。 | `stable` 候选 |
| `HOLON_HTTP_ADDR` | 运行时/控制平面命令 | 在加载 `AppConfig` 时选择本地控制平面 HTTP 地址。 | `stable` 候选 |
| `HOLON_CALLBACK_BASE_URL` | `serve`、运行时配置 | 设置回调 base URL 默认值。 | `experimental` |
| `HOLON_SOCKET_PATH` | daemon/serve | 选择本地控制 socket 路径。 | `experimental` |
| `HOLON_WORKSPACE_DIR` | 运行时命令 | 设置默认 workspace 目录。 | `experimental` |
| `HOLON_AGENT_ID` | 带可选 `--agent` / `[AGENT_ID]` 的命令 | 设置默认 agent id。 | `stable` 候选 |
| `HOLON_CONTROL_TOKEN` | `serve`、daemon、控制平面客户端配置 | 提供 bearer token/控制认证。 | `stable` 候选 |
| `HOLON_CONTROL_AUTH_MODE` | 控制平面配置 | 解析 `auto`、`required` 或 `disabled`。 | `experimental` |
| `HOLON_MODEL` | `run`、`solve`、provider 配置 | 设置默认模型；`solve --model` 会为该进程写入此环境变量。 | `stable` 候选 |
| Provider API-key 环境变量 | 由 provider 支撑的命令 | 例如 `OPENAI_API_KEY`、`ANTHROPIC_AUTH_TOKEN`，以及配置的自定义环境变量名。 | 已记录的 provider 环境变量为 `stable` 候选 |
| `RUST_LOG` 与 tracing 环境过滤器 | 所有命令 | 控制输出到 stderr 的 tracing。 | `internal` |

## 输出契约缺口

1. **面向脚本的命令现在共用规范化的 stdout 路径。** 剩余的输出工作是给每个响应形态指定 schema 归属和稳定性级别。
2. **退出码现在有基线进程契约。** 见
   [CLI 退出码](/zh-CN/reference/cli-exit-codes.md)。把特定命令的业务状态提升为非零退出仍是有意选择加入的，必须逐命令记录。
3. **机器可读输出需要 schema 归属。** CLI JSON 常镜像控制平面/运行时结构体。API 清册应决定哪些字段是稳定的、诊断的或内部的。
4. **人类输出与运维摘要混在一起。** `serve`、`run`、`solve` 和 debug 命令应明确说明其 stdout 是否脚本安全。
5. **已废弃的 `control` 仍可达。** 在记录移除计划前应保持兼容。
6. **帮助快照是手工的。** `docs/website/reference/cli.md` 声称由 `holon --help` 重新生成，但仓库中没有生成器或快照测试。

## 跟踪 issue

Milestone 8 的初始 CLI/API 稳定性后续工作已完成。第 2 阶段工作仍由
[CLI/API Stability Contracts](https://github.com/holon-run/holon/milestone/8)
里程碑通过
[#1444](https://github.com/holon-run/holon/issues/1444) 跟踪：

| 优先级 | Issue | 范围 |
|---:|---|---|
| 1 | [#1437](https://github.com/holon-run/holon/issues/1437) | 在 Milestone 8 完成后刷新 API/CLI 契约清册。 |
| 1 | [#1442](https://github.com/holon-run/holon/issues/1442) | 为稳定命令添加 JSON 输出 schema 和 golden 测试。 |
| 2 | [#1438](https://github.com/holon-run/holon/issues/1438) | 将 OpenAPI 基线迁移到 `aide` 路由/类型元数据。 |
| 2 | [#1439](https://github.com/holon-run/holon/issues/1439) | 为稳定读模型收紧 OpenAPI DTO schema。 |
| 2 | [#1440](https://github.com/holon-run/holon/issues/1440) | 添加 WorkItem 变更 HTTP 生命周期端点。 |
| 2 | [#1441](https://github.com/holon-run/holon/issues/1441) | 添加 Timer 取消生命周期端点。 |
| 3 | [#1443](https://github.com/holon-run/holon/issues/1443) | 定义稳定的面向操作者事件负载子集。 |

## 建议的下一批契约测试

| 优先级 | 测试 | 目的 |
|---:|---|---|
| 1 | 归一化空白后的 Clap 命令树/帮助快照 | 检测意外的命令/标志漂移。 |
| 1 | 针对稳定候选位置参数和别名的解析测试 | 锁定高价值 CLI 形态，而不过度快照措辞。 |
| 1 | 用假 provider 或 fixture 对 `run`/`solve` 做 `--json` 冒烟测试 | 确认机器可读模式仍可解析。 |
| 2 | `get`、`set`、`unset`、`schema`、provider remove、credential list/remove 的配置命令 golden JSON | 锁定离线脚本接口。 |
| 2 | daemon/status/log JSON 形态测试 | 锁定本地运维接口。 |
| 2 | 更多缺失令牌和命令特定业务状态的错误行为测试 | 在命令把领域失败提升为进程失败处扩展基线退出码契约。 |
| 3 | 规范或记录原始 HTTP-body 命令（`task`、`timer`、`agent create/abort`、skills add/remove/enable/disable） | 减少输出契约漂移。 |

## 后续清册范围

本 CLI 清册评审通过后，按以下顺序继续：

1. 稳定候选命令的 CLI JSON 输出 schema
   （[#1442](https://github.com/holon-run/holon/issues/1442)）。
2. CLI task/work-item/timer 命令使用的控制平面 DTO schema
   （[#1439](https://github.com/holon-run/holon/issues/1439)）。
3. tail、transcript、replay 和 SSE 使用的运行时事件负载子集
   （[#1443](https://github.com/holon-run/holon/issues/1443)）。
4. Rust crate 公开 API，如果它打算供外部使用。
