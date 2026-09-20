---
title: CLI 参考
summary: Holon 命令行界面——基于 holon --help 验证（v0.44.1）。
order: 10
---
<!-- maintenance: regenerate from `holon --help` output when commands change. Last regenerated against v0.44.1. -->

# CLI 参考

Holon 的命令行界面。所有命令都接受 `--help`，用于查看详细的参数文档。

脚本编写指导、稳定性级别和支持策略见
[CLI 稳定性策略](/zh-CN/reference/cli-stability-policy.md)和
[CLI 契约清单](/zh-CN/reference/cli-contract-inventory.md)。

## 命令树

```text
holon (v0.44.1)
├── context      显示本次 CLI 调用的声明式调用方上下文
├── commands     显示机器可读的 CLI 命令元数据
├── serve        启动 HTTP 控制平面服务
├── onboard      交互式配置向导或对密钥安全的诊断
├── daemon       后台 daemon 生命周期
│   ├── start    启动 daemon
│   ├── prepare-update 停止 daemon 且不改变期望的自启状态
│   ├── stop     停止 daemon
│   ├── status   查看 daemon 状态
│   ├── restart  重启 daemon
│   └── logs     查看 daemon 日志
├── config       运行时配置
│   ├── get      读取配置键
│   ├── set      写入配置键
│   ├── unset    删除配置键
│   ├── providers 提供商管理
│   │   ├── set    添加/更新提供商
│   │   ├── get    显示提供商
│   │   ├── list   列出所有提供商
│   │   ├── remove 删除提供商
│   │   └── doctor 提供商凭据检查
│   ├── credentials API key 存储
│   │   ├── set    存储凭据
│   │   ├── list   列出已存储的凭据
│   │   └── remove 删除凭据
│   ├── models  模型目录与发现
│   │   ├── list    列出可用模型
│   │   └── refresh 刷新某提供商已发现的模型
│   ├── migrate-model-routes  查看/改写旧的模型选择
│   ├── list     列出当前全部配置
│   ├── schema   显示所有配置键的类型和默认值
│   └── doctor   完整系统健康检查
├── prompt       向 Agent 发送提示词（轻量）
├── tail         显示最近的日志末尾
├── transcript   显示对话记录
├── events       读取稳定的运行时事件信封
│   ├── tail     拉取一页有界的事件信封
│   └── stream   以换行分隔 JSON 流式输出事件信封
├── task         把命令作为后台任务运行
│   ├── list     列出任务
│   ├── run      把命令作为受管后台任务运行
│   ├── status   显示任务生命周期状态
│   ├── output   读取任务输出
│   ├── input    向任务发送文本输入
│   └── stop     停止任务
├── work-item    查看和管理 WorkItem
│   ├── list     列出 WorkItem
│   ├── get      显示 WorkItem
│   ├── create   创建 WorkItem
│   ├── pick     把 WorkItem 选为当前焦点
│   ├── update   更新 WorkItem
│   └── complete 完成 WorkItem
├── timer        创建、列出或取消定时器
│   ├── create   创建延时或周期性定时器
│   ├── list     列出活跃定时器
│   └── cancel   取消活跃定时器
├── control      [已废弃] 请改用 `holon agent start|stop|abort`
├── agent        Agent 管理
│   ├── list     列出所有 Agent
│   ├── get      显示规范 Agent 详情
│   ├── status   显示 Agent 状态
│   ├── create   创建新 Agent
│   ├── rename   重命名公开自属 Agent
│   ├── repair   重试创建后未完成的引导步骤
│   ├── start    启动 Agent
│   ├── stop     停止 Agent
│   ├── delete   永久删除 Agent 及其数据
│   ├── abort    中止当前运行
│   ├── reset-callback 重置 Agent 的外部触发器回调
│   └── model    按 Agent 的模型配置
│       ├── get  获取 Agent 模型覆盖
│       ├── set  设置 Agent 模型覆盖
│       └── clear 清除 Agent 模型覆盖
├── skills         管理 skill
│   ├── catalog    列出 Skill Library 目录
│   ├── add        向库中添加 skill
│   ├── remove     从库中删除 skill
│   ├── check      检查库一致性
│   ├── reconcile  按 lock 文件对齐库
│   ├── list       列出 Agent 已启用的 skill
│   ├── enable     为 Agent 启用 skill
│   ├── disable    为 Agent 禁用 skill
│   ├── update     从远程源拉取并更新 skill
│   ├── refresh    重新扫描本地根目录
│   ├── install    [已废弃] 兼容别名
│   └── uninstall  [已废弃] 兼容别名
├── run          单次 Agent 交互
├── solve        处理 GitHub issue 或类似目标
├── workspace    工作区管理（attach、exit、detach）
│   ├── attach   附加到已有工作区
│   ├── exit     退出当前工作区
│   └── detach   从工作区分离
├── tui          启动交互式终端 UI
├── memory-index 记忆索引管理
│   └── rebuild  重建记忆搜索索引
├── models-dev   models.dev 快照刷新、校验与审计
│   ├── refresh  拉取快照并重新生成产物
│   ├── validate 校验检入的快照和产物
│   └── audit    按快照审计提供商映射
├── debug        调试工具
│   ├── prompt   调试模式提示词
│   ├── latency  显示延迟指标
│   ├── performance  显示性能指标
│   ├── trace    按 id 或搜索查看端到端 trace
│   ├── runtime-db   运行时数据库审计、保留与维护
│   │   ├── agent-relations 报告或回填规范 Agent 关系记录
│   │   ├── audit    审计运行时数据库不变式
│   │   ├── retention 对历史数据库记录执行保留清理
│   │   ├── compact  压缩运行时数据库
│   │   ├── wait-final-brief-publication 准备或应用 WaitFor final 简报关联修复
│   │   ├── turn-settlement 审计或应用针对历史终端 Turn 结算的指纹隔离修复
│   │   └── conversation-input-assignment-rollback 预检或回滚 v66 修复标记
│   ├── scheduler-recovery  查看/应用调度器恢复
│   └── scheduler-fixture 生成调度器夹具数据
└── help         打印帮助
```

> **注意：** 本参考基于仓库中检入的 CLI 快照维护。如果你运行的是从 `main`
> 构建的源码版本，部分命令或参数可能不同。请始终用 `holon --help` 和
> `holon <COMMAND> --help` 查看你所安装版本的实时命令参考。

## 常见工作流

### 单次快速调用

```bash
holon run "Explain Rust ownership"
holon run --json "List files"                          # JSON 输出
holon run --authority-class external-evidence "User query"  # 设置权限类别
```

### 创建并使用 Agent

```bash
holon agent create reviewer --template code-reviewer
holon agent repair reviewer
holon run --agent reviewer "Review src/runtime/turn.rs"
```

### Agent 生命周期

```bash
holon agent start reviewer
holon agent stop reviewer
holon agent abort reviewer
holon agent delete reviewer --yes
```

> **已废弃：** `holon control` 命令已被 `holon agent start`、`holon agent stop`
> 和 `holon agent abort` 取代。旧的 `control` 命令仅为向后兼容而保留；兼容性
> 和移除标准见
> [CLI 稳定性策略](/zh-CN/reference/cli-stability-policy.md#deprecated-holon-control)。

`holon agent delete` 会永久删除一个 Agent 及其关联数据。传入
`--cascade-private-children` 可一并删除它的私有子 Agent，传入 `--wait` 会阻塞
直到删除任务完成。非交互模式下需要 `--yes`。

`holon agent repair <AGENT_ID>` 会重试创建时未完成的模板、运行时、工作区、
模型和初始消息步骤。它不会重建 Agent，也不会覆盖冲突的用户管理状态。

`holon agent rename <AGENT_ID> --name <NAME>` 会更新公开自属 Agent 的显示名，
并回显更新后的 Agent 详情。Agent id 是永久的；已配置的默认 Agent 不能重命名，
重名会以可读的冲突错误拒绝。

### 模型选择

```bash
holon config set model.default "deepseek-anthropic@default/deepseek-v4-pro"
holon agent model set "anthropic@default/claude-sonnet-4-6" reviewer
holon agent model get reviewer
holon agent model clear reviewer
```

可执行的模型选择使用规范的 `provider@endpoint/model` 路由引用。旧的
`provider/model` 输入仍然接受。用以下命令查看或改写已持久化的旧值：

```bash
holon config migrate-model-routes          # 预演
holon config migrate-model-routes --write  # 经校验的规范化改写
```

### models.dev 提供商映射

Holon 附带一份检入仓库的 [models.dev](https://models.dev) 快照，以及一份带版本
的提供商映射清单，用来把上游模型元数据与 Holon 的提供商/路由标识对齐。
`holon models-dev` 的各子命令负责审计、校验和刷新这份快照：

```bash
holon models-dev validate        # 校验检入的快照和产物
holon models-dev audit           # 按快照审计提供商映射
holon models-dev audit --json    # 机器可读的映射审计报告
holon models-dev refresh         # 拉取上游并重新生成产物
```

`refresh` 和 `validate` 面向仓库中检入的 `models.dev/` 文件，供 Holon 开发和
发布自动化使用。运行时模型目录见[支持的模型](/zh-CN/reference/models.md)。

### 记忆索引管理

管理面向 Agent 和工作区的本地向量与全文检索记忆索引：

```bash
holon memory-index rebuild                      # 向后台索引器提交全量重建任务
holon memory-index rebuild --agent <AGENT>      # 重建指定 Agent 的记忆索引
holon memory-index rebuild --workspace <WS>     # 重建指定工作区的记忆索引
holon memory-index rebuild --offline            # 离线直接执行，无需提交至 daemon
```

### 调试与运维工具

Holon 在 `holon debug` 下提供一系列用于检查运行时指标、Trace 追踪与数据库健康状态的运维子命令：

```bash
# 性能、延迟与 Trace 诊断
holon debug latency
holon debug performance
holon debug trace <trace_id>

# 运行时数据库审计、压缩与修复
holon debug runtime-db audit
holon debug runtime-db compact
holon debug runtime-db retention
holon debug runtime-db agent-relations
holon debug runtime-db wait-final-brief-publication --dry-run
holon debug runtime-db turn-settlement

# 调度器状态诊断与恢复
holon debug scheduler-recovery
holon debug scheduler-fixture
```

### Daemon 管理

```bash
holon daemon start
holon daemon start --port 8787 --access tunnel
holon daemon status
holon daemon logs
holon daemon restart
holon daemon stop
```

### 初始化配置

`holon onboard` 是首次配置 Holon，或修复损坏的提供商/模型配置的最快方式。
它有两种模式：

- **交互式 TUI**（在终端中默认启用）：引导你依次完成提供商选择、模型选择、搜索
  设置和凭据输入，并且不会把密钥内容回显到屏幕上。
- **JSON 诊断**（`--json` 或非 TTY）：打印一份对密钥安全的诊断报告，并给出可执行
  的后续步骤，适合脚本和 CI。

```bash
holon onboard                    # 交互式配置向导（TTY）
holon onboard --json             # 对密钥安全的诊断报告（JSON）
```

TUI 流程会依次引导你完成：

1. **Provider** — 从内置和自定义提供商中选择
2. **Credential** — 对 OpenAI Codex：用浏览器完成 OAuth 登录；对其他提供商：
   输入你的 API key（输入内容不会被回显，也不写入日志）
3. **Model** — 为所选提供商选择默认模型，或输入自定义模型 id
4. **Search** — 启用 DuckDuckGo 托管搜索、模型原生搜索，或关闭搜索
5. **Apply** — 写入配置、存储凭据并打印摘要

JSON 报告包含 `status`、`sections`（home、agent、model_provider、search、
credentials）和 `next_actions`。它在设计上对密钥安全：报告中不会出现任何凭据
内容。

### 配置查看

```bash
holon config list                # 当前全部配置
holon config schema              # 所有配置键及其类型和默认值
holon config doctor              # 完整健康检查
holon config providers list      # 所有已注册的提供商
holon config models list         # 可用模型及其状态
holon config credentials list    # 已存储的凭据配置
```

目前稳定的、面向脚本的 JSON 契约覆盖 `holon config schema`、
`holon config providers remove` 和 `holon config credentials set/list/remove`。
其他配置查看命令也会输出 JSON，但在其提供商/运行时 DTO 归属完全稳定之前仍属
实验性。人类可读的帮助和文本输出与这些 JSON 契约相互独立。

### 凭据设置

```bash
holon config credentials set --kind api_key --stdin deepseek
# 粘贴密钥，按 Enter，然后按 Ctrl+D
holon config credentials remove deepseek
```

### 自定义提供商

```bash
holon config providers set my-proxy \
  --transport anthropic_messages \
  --base-url "https://my-proxy.example.com" \
  --credential-source env \
  --credential-env "MY_PROXY_API_KEY" \
  --credential-kind api_key
```

### HTTP 服务端

```bash
holon serve --port 8787
holon serve --port 8787 --token "secret"
holon serve --access tunnel
```

### 后台任务

```bash
holon task run "Build project" --cmd "cargo build"
holon task status <TASK_ID>
holon task output <TASK_ID> --block --timeout-ms 30000
holon task input <TASK_ID> --text "continue\n"
holon task stop <TASK_ID>
```

任务生命周期命令默认作用于已配置的默认 Agent。传入 `--agent <AGENT>` 可以查看
或控制属于其他公开 Agent 的任务。所有任务生命周期命令都会打印对应的 JSON 控制
平面或读模型响应。

### WorkItem

```bash
holon work-item list
holon work-item list --limit 10 --agent planner
holon work-item get <WORK_ITEM_ID>
holon work-item get <WORK_ITEM_ID> --agent planner
holon work-item create "Triage failing CI"
holon work-item pick <WORK_ITEM_ID> --reason "unblock release"
holon work-item complete <WORK_ITEM_ID>
```

`list` 和 `get` 为只读，打印 `/agents/:agent_id/work-items` 和
`/agents/:agent_id/work-items/:work_item_id` 返回的 HTTP 读模型
`WorkItemRecord` JSON 结构。`create`、`update`、`pick` 和 `complete` 子命令会
修改 WorkItem 状态，并返回对应的控制平面响应。

### 定时器

```bash
holon timer create --after-ms 60000 --summary "心跳检查"
holon timer list
holon timer cancel <TIMER_ID>
```

`holon timer` 用于为 Agent 调度延时或周期性定时器（默认为默认 Agent，或通过 `--agent <AGENT>` 指定）。

### 事件

```bash
holon events tail --limit 20
holon events tail --order asc --max-level info
holon events tail --agent benchmark-run --order asc --offline
holon events stream --after-seq 42 --max-events 100
```

`events tail --offline` 会从本地运行时数据库读取同样稳定的事件信封，无需运行中
的 daemon。离线分页不支持 `--max-level`。

### 终端 UI

```bash
holon tui
holon tui --no-alt-screen
holon tui --connect http://remote:8787 --token "secret"
```

### 多轮任务

```bash
holon run --max-turns 5 "Write a Rust function with tests"
holon run --workspace-root /path/to/project "Analyze this codebase"
holon run --agent builder --workspace-root /path/to/project "Fix build errors"
```

## 关键参数参考

### `holon run` 参数

| 参数 | 说明 |
|--------|-------------|
| `--agent <AGENT>` | 指定目标 Agent |
| `--create-agent` | Agent 不存在时创建 |
| `--template <TEMPLATE>` | 新 Agent 使用的 Agent 模板 |
| `--authority-class <CLASS>` | 权限类别：`operator-instruction`、`runtime-instruction`、`integration-signal`、`external-evidence`（别名：`--trust`） |
| `--json` | 机器可读的 JSON 输出 |
| `--max-turns <N>` | 限制 Agent 轮次 |
| `--no-wait-for-tasks` | 不阻塞等待后台任务 |
| `--workspace-root <PATH>` | 工作区根目录 |
| `--cwd <PATH>` | 工作目录 |
| `--home <PATH>` | Holon home 目录 |

### `holon serve` 参数

| 参数 | 说明 |
|--------|-------------|
| `--port <PORT>` | 监听端口 |
| `--host <HOST>` | 绑定主机 |
| `--listen <ADDR>` | 监听地址 |
| `--access <MODE>` | `local`、`tunnel`、`lan`、`tailnet` |
| `--token <TOKEN>` | 用于鉴权的 Bearer token |
| `--token-file <PATH>` | 从文件读取 token |
| `--advertise <URL>` | 对外公布的 URL |
| `--desktop-integration[=true\|false]` | 开启 Finder 操作；仅支持 macOS 和回环监听，默认关闭 |

### `holon daemon start` 参数

| 参数 | 说明 |
|--------|-------------|
| `--port <PORT>` | Daemon 端口 |
| `--access <MODE>` | 访问模式（与 serve 相同） |
| `--host <HOST>` | 绑定主机 |
| `--listen <ADDR>` | 监听地址 |
| `--token <TOKEN>` | 鉴权 token |
| `--desktop-integration[=true\|false]` | 与 serve 相同；restart 继承设置，除非显式覆盖 |

### `holon agent create` 参数

| 参数 | 说明 |
|--------|-------------|
| `--template <TEMPLATE>` | 内置模板或路径模板 |

### `holon solve` 参数

| 参数 | 说明 |
|--------|-------------|
| `--repo <REPO>` | 目标仓库 |
| `--workspace <PATH>` | 工作区目录 |

## 另见

- [配置参考](/zh-CN/reference/configuration.md) — 配置键与凭据管理
- [HTTP 控制平面](/zh-CN/reference/http-control-plane.md) — HTTP API 设计理念
- [入门指南](/zh-CN/getting-started/first-agent.md) — 配置教程
- [快速示例](/zh-CN/guides/quick-examples.md) — 面向任务的示例
