---
title: 配置
summary: Holon 的配置文件、配置键、凭据、环境变量与诊断。
order: 15
---
<!-- maintenance: hand-written; verify against `holon config schema` and `holon config list` when config keys change. Last verified against v0.45.0. -->

# 配置参考

Holon 把运行时配置保存在 `~/.holon/` 下的 JSON 文件中：

| 文件 | 用途 |
|------|------|
| `~/.holon/config.json` | 提供商、默认模型、TUI、Web 与运行时设置 |
| `~/.holon/credentials.json` | 受权限保护的凭据存储（通过 `config credentials` 管理） |

## 配置键

用 `holon config get/set/unset/list` 读写配置键。本地 daemon 运行时，这些命令优先走
daemon 的运行时配置 API；无法连接 daemon 时，回退到离线配置存储。`set` 和 `unset` 会在
stderr 打印 `applied_via=daemon_api` 或 `applied_via=offline_store`，stdout 仍是脚本
使用的 JSON 值或状态。不被支持的 daemon 更新会失败，并给出 daemon 返回的拒绝原因。被
接受的 daemon 更新会持久化到 `config.json`；在支持重启或重载之前，运行中的 daemon 可能
继续使用当前生效的配置，CLI 会在 stderr 上提示这一点。
用 `holon config schema` 查看所有可用的键及其类型、默认值和说明。

### 模型与提供商设置

| 键 | 类型 | 说明 |
|-----|------|-------------|
| `vision.default` | model_route_ref_or_auto | ViewImage 视觉观测的路由引用。未设置时自动发现支持图像的提供商 |
| `image_generation.default` | model_route_ref_or_auto | GenerateImage 请求的路由引用。未设置时选择第一个支持图像生成的轮次模型 |
| `model.default` | model_route_ref | 默认可执行路由，例如 `"anthropic@default/claude-sonnet-4-6"` |
| `model.fallbacks` | model_route_ref_list | 有序的可执行回退路由 |
| `runtime.disable_provider_fallback` | boolean | 禁用提供商/模型回退，要求确定性的单提供商执行 |

```bash
# 设置默认模型
holon config set model.default "deepseek-anthropic@default/deepseek-v4-pro"

# 添加回退模型（JSON 数组）
holon config set model.fallbacks '["anthropic@default/claude-sonnet-4-6","minimax@default/MiniMax-M2.7"]'

# 读取当前默认值
holon config get model.default

# 查看全部当前配置
holon config list

# 删除配置键（恢复默认值）
holon config unset model.fallbacks
```

### 按模型策略

`models.catalog` 键用于覆盖特定提供商/模型引用的运行时元数据。`model.unknown_fallback.*`
下的键控制缺少内置元数据的模型的策略。

模型元数据与可执行选择使用不同的身份：

- `provider/model` 是逻辑模型引用，供 `models.catalog` 使用。
- `provider@endpoint/model` 是模型路由引用，供默认值、回退、视觉与图像生成选择以及
  Agent 覆盖使用。

旧的 `provider/model` 选择值仍被接受，但所有新的写入都会带上 endpoint。用以下命令检查
或显式重写现有配置与 Agent 状态：

```bash
holon config migrate-model-routes          # 试运行
holon config migrate-model-routes --write  # 经校验的规范化重写
```

写入会创建一次性配置备份，并在一个 SQLite 事务中更新所有 Agent 状态。无效或有歧义的
引用会阻止部分写入。

### 认证与 Session 设置

Holon 控制平面支持基于 Cookie 的 Session 认证（适用于浏览器和 Web UI 客户端）以及 Bearer Token 认证。

| 键 | 类型 | 默认值 | 说明 |
|-----|------|---------|-------------|
| `auth.mode` | string (`"local"` \| `"oidc"`) | `"local"` | 认证模式。`"local"` 支持 Bearer Token 与本地 Cookie Session；`"oidc"` 启用 OpenID Connect 登录流程。变更需重启 daemon 生效。 |
| `auth.oidc.issuer_url` | string | unset | OIDC Issuer 发现 URL（如 `https://auth.example.com/realms/holon`） |
| `auth.oidc.client_id` | string | unset | 向 Issuer 注册的 OIDC 客户端 ID |
| `auth.oidc.client_secret_env` | string | unset | 包含 OIDC 客户端密钥的环境变量名 |
| `auth.oidc.redirect_uri` | string | unset | 回调重定向 URI（如 `http://localhost:7878/api/auth/oidc/callback`） |
| `auth.session.absolute_ttl_seconds` | positive_integer_or_null | unset (`null`) | 绝对 Session 生命周期（秒，`null` 表示无绝对上限） |
| `auth.session.idle_ttl_seconds` | positive_integer | `86400`（24小时） | 空闲超时时间（秒），无交互超过该时长后 Session 失效 |

有关从 IdP 注册到会话验证的完整步骤，请参阅[配置 OIDC 身份认证](/zh-CN/guides/configure-oidc-authentication.md)。

> **Session 生命周期约束：**
> - 配置 `auth.session.absolute_ttl_seconds` 时，其数值必须大于或等于 `auth.session.idle_ttl_seconds`。
> - 将 `auth.session.absolute_ttl_seconds` 设为 `null`（或不设置）表示不限制绝对超时，活跃会话将持续有效。为保持兼容性，配置中的 `0` 会被自动归一化为 `null`。
> - 生产环境下 Issuer URL 与回调地址必须使用 HTTPS（仅本地测试允许 `localhost` 使用 HTTP）。
> - 变更 `auth.mode` 或 OIDC 参数后需要重启 daemon 生效。

```bash
# 配置 Session 超时
holon config set auth.session.idle_ttl_seconds 43200

# 启用 OIDC 认证
holon config set auth.mode "oidc"
holon config set auth.oidc.issuer_url "https://auth.example.com/realms/holon"
holon config set auth.oidc.client_id "holon-client"
holon config set auth.oidc.client_secret_env "HOLON_OIDC_CLIENT_SECRET"
```

### HTTP API CORS

CORS 默认对任意端口的 localhost/loopback 浏览器来源启用：
`http://localhost:<port>`、`https://localhost:<port>`、
`http://127.0.0.1:<port>` 和 `http://[::1]:<port>`。这样本地 Web UI 在提供所需的
`Authorization: Bearer <token>` 请求头时，就能调用本地或远程的 Holon HTTP/控制 API。

配置 `api.cors.allowed_origins` 可以添加非本地的浏览器来源，例如局域网托管的 Web UI。
这些来源会追加到内置的 localhost/loopback 白名单。局域网访问还要求 API 绑定到可达的
地址，例如 `0.0.0.0:7878` 或特定的局域网 IP；绑定到 `127.0.0.1` 时其他设备无法访问。
设置 `api.cors.enabled=false` 可完全禁用 CORS。

```bash
holon config set api.cors.allowed_origins '["http://192.168.1.10:5173"]'
holon config set api.cors.allowed_methods '["GET","POST","PATCH","DELETE","OPTIONS"]'
holon config set api.cors.allowed_headers '["content-type","authorization"]'
holon config set api.cors.allow_credentials false
holon config set api.cors.max_age_seconds 600
```

不要同时设置 `api.cors.allow_credentials=true` 和
`api.cors.allowed_origins=["*"]`；Holon 会拒绝这个不安全的组合。

### Projection Gate

| 键 | 类型 | 默认值 | 说明 |
|-----|------|---------|-------------|
| `api.projection.max_leaders` | integer | `16` | 并发 projection 构建的上限；在 leader 释放前，更多不同 key 会得到 `429 projection_busy` |
| `api.projection.cache_ttl_ms` | integer | `500` | 已完成的 projection 构建被缓存并复用的毫秒数 |

### 调度器

规范调度器始终启用。`runtime.scheduler` 不再是可配置的键。为兼容一个次版本，已有的
持久化值 `runtime.scheduler=canonical` 或环境变量值 `HOLON_SCHEDULER=canonical` 会被
接受，并给出弃用警告。`legacy` 及其他所有值都会导致启动失败。请从部署配置中移除这个
过时的选择器。

迁移 40 把 rollout 表标记为已废弃的兼容数据。后续清理迁移会在恢复到达固定点后删除这些
表。它们不参与启动、常规调度器事务或类型化修复中的权限判定。

当存在未结束的规范执行、在途的执行工作项或已出队的队列条目时，后续清理迁移会失败关闭。
错误信息会列出受影响的 Agent ID。停止 Holon，运行
`holon debug scheduler-recovery --agent <agent>` 进行报告、应用类型化恢复，然后再次
报告。该命令可以在不触发清理的情况下打开紧邻的前一个 schema；当前二进制的其他命令做
不到。

对于仍需要旧调度器的部署，Holon v0.31.1 是回滚版本。仅在拥有迁移前数据库备份时使用它；
经后续 schema 清理迁移过的数据库不支持降级。

### Decision 子系统配置

| 键 | 类型 | 默认值 | 说明 |
|-----|------|---------|-------------|
| `decision.enabled` | boolean | `false` | 启用可选的 Decision 提供者子系统 |
| `decision.model` | model_route_ref | unset | Decision 使用的共享提供者模型路由；所选模型必须显式声明 Decision 能力 |
| `decision.local_onnx.enabled` | boolean | `false` | 启用嵌入式本地 ONNX 决策提供者 |
| `decision.local_onnx.preset` | string | `jev-selector-q4f16` | 内置本地 ONNX 模型预设名称 |
| `decision.local_onnx.model_dir` | string | unset | 包含 ONNX 模型文件和 manifest 的本地目录 |
| `decision.local_onnx.variant` | string | `q4f16` | 模型量化或变体标识 |
| `decision.local_onnx.num_threads` | integer | `1` | 本地 ONNX 推理使用的 CPU 线程数 |
| `decision.local_onnx.checksum` | string | unset | 模型目录的可选 SHA-256 校验和 |
| `decision.timeout_ms` | integer | unset | 提供者请求超时时间（毫秒） |
| `decision.max_tokens` | integer | unset | Decision 响应的最大输出 Token 数 |
| `decision.concurrency` | integer | unset | 最大并发 Decision 请求数 |
| `decision.queue_capacity` | integer | unset | 最大排队 Decision 请求数 |
| `decision.tools.enabled` | boolean | `false` | 向 Agent 暴露非权威决策工具（`AdvisoryDecision`） |
| `decision.tools.max_calls_per_turn` | integer | unset | 单轮对话允许的最大咨询工具调用次数（未设置表示不限） |
| `decision.tools.timeout_ms` | integer | unset | 咨询工具超时时间（毫秒） |
| `decision.tools.min_confidence` | float | unset | 最小置信度阈值（0.0 至 1.0）；低于该阈值时显式弃权（abstain） |

```bash
# 启用 Decision 子系统与咨询工具
holon config set decision.enabled true
holon config set decision.tools.enabled true

# 将决策路由至专用模型
holon config set decision.model "typesafe@default/typesafe-ai/jev"

# 或启用零外发的本地 ONNX 决策提供者
holon config set decision.local_onnx.enabled true
holon config set decision.local_onnx.preset "jev-selector-q4f16"

# 设置安全防护上限
holon config set decision.tools.max_calls_per_turn 3
holon config set decision.tools.min_confidence 0.65
```

## 凭据管理

凭据安全存储在 `~/.holon/credentials.json`。使用 `config credentials` 子命令，**不要直接编辑这个文件**。

### 设置凭据

```bash
# 推荐：用 --stdin 避免 shell 历史泄露
holon config credentials set --kind api_key --stdin deepseek
# 粘贴 API key 后回车（Ctrl+D 结束）

# 备选：--material（会出现在 shell 历史中，不推荐）
holon config credentials set --kind api_key --material "sk-..." deepseek
```

`<PROFILE>` 参数是你自选的标签（例如 `deepseek`、`bigmodel`、`openai`）。

### 列出与删除

```bash
holon config credentials list
holon config credentials remove deepseek
```

### 环境变量

作为凭据存储的替代方案，Holon 从环境变量读取 API key：

| 提供商 | 环境变量 |
|----------|---------------------|
| Anthropic | `ANTHROPIC_AUTH_TOKEN` |
| DeepSeek | `DEEPSEEK_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| BigModel (Zhipu) | `BIGMODEL_API_KEY` |
| MiniMax | `MINIMAX_API_KEY` |
| Xiaomi MiMo | `XIAOMI_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| Fireworks | `FIREWORKS_API_KEY` |
| Together | `TOGETHER_API_KEY` |
| Mistral | `MISTRAL_API_KEY` |
| xAI | `XAI_API_KEY` |
| Moonshot | `MOONSHOT_API_KEY` |
| NEAR AI Cloud（TEE 推理） | `NEARAI_API_KEY` |
| Volcengine | `VOLCENGINE_API_KEY` 或 `ARK_API_KEY` |
| StepFun | `STEPFUN_API_KEY` |
| Qwen | `QWEN_API_KEY` 或 `DASHSCOPE_API_KEY` |
| HuggingFace | `HUGGINGFACE_API_KEY` 或 `HF_TOKEN` |
| Venice | `VENICE_API_KEY` |
| Chutes | `CHUTES_API_KEY` |
| NVIDIA | `NVIDIA_API_KEY` |

完整且最新的列表请运行 `holon config providers list`。

### 凭据来源

| 来源 | 说明 |
|--------|-------------|
| `none` | 不需要凭据（仅本地提供商） |
| `env` | 从环境变量读取凭据 |
| `credential_profile` | 按 profile 名从 `~/.holon/credentials.json` 读取凭据 |
| `external_cli` | 运行外部 CLI（例如 `codex`）获取凭据 |

## 提供商配置

Holon 内置 40+ 个提供商的定义。你可以在 `config.json` 中添加或覆盖提供商。

### 列出已注册提供商

```bash
holon config providers list
```

每个提供商条目会显示它的 transport 协议（`anthropic_messages`、`openai_chat_completions`
等）、base URL 和凭据要求。

### Ollama（本地）

Holon 内置 [`ollama`](/zh-CN/reference/models.md) 提供商，用于通过
[Ollama](https://ollama.com) 在本地运行模型。它不需要 API key，通过 Anthropic Messages
transport 连接本地 Ollama 服务器 `http://127.0.0.1:11434`。

1. 安装并启动 Ollama，然后拉取模型，例如 `ollama pull qwen3.8:latest`。
2. 在 `holon onboard` 中选择 Ollama，或直接设置：

```bash
holon config set model.default "ollama/qwen3.8:latest"
```

Web GUI 无需任何凭据配置即可发现本地运行的 Ollama 模型，`ViewImage` 也会自动发现 Ollama
视觉模型用于图像分析。

### 添加自定义提供商

```bash
holon config providers set my-proxy \
  --transport openai_chat_completions \
  --base-url "https://my-proxy.example.com/v1" \
  --credential-source env \
  --credential-env "MY_PROXY_API_KEY" \
  --credential-kind api_key
```

选项：
- `--transport`：协议，取值为 `anthropic_messages`、`openai_chat_completions` 或 `openai_responses`
- `--base-url`：API 端点基础 URL
- `--credential-source`：`none`、`env`、`credential_profile` 或 `external_cli`
- `--credential-kind`：`none`、`api_key` 或 `session_token`
- `--credential-env`：环境变量名（当 source 为 `env` 时）
- `--credential-profile`：凭据存储 profile（当 source 为 `credential_profile` 时）

### 提供商端点与 Plan

提供商配置把提供商账号与其具体端点分开。旧版提供商键仍用于配置默认端点：

```bash
holon config set providers.openai.transport openai_responses
holon config set providers.openai.base_url "https://api.openai.com/v1"
```

它们是 `providers.openai.endpoints.default.transport` 和
`providers.openai.endpoints.default.base_url` 的快捷方式。当同一提供商账号需要另一个
transport、base URL 或凭据策略时，使用端点键：

```bash
holon config set providers.volcengine.endpoints.image-openai.transport openai_chat_completions
holon config set providers.volcengine.endpoints.image-openai.base_url "https://ark.cn-beijing.volces.com/api/plan/v3"
holon config set providers.volcengine.plans.image-openai.endpoint image-openai
```

plan 把稳定的提供商别名（例如 `volcengine-image-openai`）映射到具名端点。规范化选择会
显式持久化该路由，例如 `volcengine@image-openai/model-id`。已有的内置别名和更旧的
`provider/model` 引用继续作为兼容输入可用。

### 删除提供商

```bash
holon config providers remove my-proxy
```

## 提供商 OAuth 与登录流程

部分提供商使用 OAuth 或浏览器登录，而不是静态 API key。Holon 支持两种 OAuth 风格流程：

### OpenAI Codex OAuth

Codex 支持两种 OAuth 流程：

- **设备 OAuth（推荐）**：Holon 请求设备码，打印验证 URL 和用户码。在任意浏览器中打开
  该 URL 并输入用户码，daemon 会自动轮询完成状态。整个流程作为后台任务运行（见 Web GUI
  的任务监控）。
- **浏览器 OAuth**：`codex` CLI 打开浏览器进行 OAuth 登录，并把凭据存储在本地。Holon
  通过 `external_cli` 读取它。

浏览器 OAuth 使用 `credential_source: external_cli` 和 `credential_kind: oauth`；设备
OAuth 则通过引导向导完成。

如果凭据过期，再次运行 `holon onboard`：向导会检测到过期并引导你重新认证。

### Vercel AI Gateway OIDC

Vercel AI Gateway 使用 OpenID Connect（OIDC）进行认证。当选择 Vercel 作为提供商时，
引导向导支持这一流程。

对于 OAuth 和 OIDC 流程，推荐的设置路径是：

```bash
holon onboard
```

向导会处理整个 OAuth 流程并安全存储凭据。除非你在编写无头部署脚本，否则不要尝试在
`config.json` 中手动配置 OAuth 提供商。

## 列出可用模型

```bash
holon config models list
```

它会显示每个模型的可用性、凭据状态、提供商、transport 和策略（上下文窗口、最大输出
token 数、能力）。

## Agent 级模型覆盖

每个 Agent 都可以覆盖默认模型：

```bash
holon agent model set "anthropic@default/claude-sonnet-4-6" reviewer
```

该覆盖存储在 Agent 自己的配置中，而不是全局的 `model.default`。

## 诊断

```bash
# 完整系统健康检查，包括模型可用性
holon config doctor

# 列出所有配置键及其类型和默认值
holon config schema
```

`config doctor` 报告：默认模型、回退模型、逐模型可用性、提供商设置和重试策略。

## 配置文件位置

Holon 按以下顺序解析配置目录：

1. `$HOLON_HOME/config.json`（设置了 `HOLON_HOME` 时）
2. `~/.holon/config.json`（回退）

凭据遵循同样的规则，使用 `credentials.json`。

## TUI 设置

| 键 | 取值 | 默认值 | 说明 |
|-----|--------|---------|-------------|
| `tui.alternate_screen` | `auto`、`always`、`never` | `auto` | 备用屏幕缓冲区行为 |

TUI 调试探针由环境变量控制：

| 环境变量 | 取值 | 默认值 | 说明 |
|----------------------|--------|---------|-------------|
| `HOLON_TUI_PRESENTATION_LOG` | `1`、`true`、`yes`、`on`、`debug` | unset | 为流驱动的呈现决策启用 `<HOLON_HOME>/logs/tui/presentation.jsonl` 调试日志 |
| `HOLON_TUI_PRESENTATION_LOG_MAX_BYTES` | 正整数，单位字节 | `5242880` | 当呈现调试日志达到该大小时轮转 |

## 运行时可观测性

OTLP trace 导出是可选的，默认关闭。控制 API 运行时，OpenMetrics 通过受保护的
`/api/control/runtime/metrics` 端点暴露。

| 键 | 类型 | 默认值 | 说明 |
|-----|------|---------|-------------|
| `runtime.observability.otlp.enabled` | boolean | `false` | 启用有界的 OTLP/HTTP JSON trace 导出器 |
| `runtime.observability.otlp.endpoint` | string | unset | 完整的 HTTP 或 HTTPS OTLP trace 端点，通常以 `/v1/traces` 结尾 |
| `runtime.observability.otlp.headers` | json_object | `{}` | 静态的非机密请求头 |
| `runtime.observability.otlp.credential_profile` | string | unset | 作为 bearer authorization 头注入的凭据 profile |
| `runtime.observability.otlp.queue_capacity` | positive integer | `1024` | 非阻塞导出器队列中等待的最大 span 数 |
| `runtime.observability.otlp.batch_size` | positive integer | `128` | 单个导出请求中的最大 span 数 |
| `runtime.observability.otlp.batch_interval_ms` | positive integer | `1000` | 最大批处理延迟 |
| `runtime.observability.otlp.timeout_ms` | positive integer | `5000` | 每个请求的导出超时 |

OTLP 设置在 daemon 启动时生效。Collector、Prometheus、Grafana、告警和故障排查示例见
[运行时可观测性指南](/zh-CN/reference/observability)。

## Web 抓取/搜索设置

| 键 | 类型 | 默认值 | 说明 |
|-----|------|---------|-------------|
| `web.fetch.enabled` | boolean | `true` | 启用 WebFetch 工具 |
| `web.fetch.max_chars` | integer | `20000` | 返回给模型的最大字符数 |
| `web.fetch.max_response_bytes` | integer | `750000` | 截断前的最大响应字节数 |
| `web.fetch.timeout_seconds` | integer | `20` | 每个请求的超时 |
| `web.fetch.max_redirects` | integer | `5` | 最大重定向跳数 |
| `web.fetch.allowed_hosts` | string_list | `[]` | 允许的主机（为空表示全部） |
| `web.fetch.denied_hosts` | string_list | `[]` | 被屏蔽的主机 |
| `web.search.enabled` | boolean | `true` | 启用 WebSearch 工具 |
| `web.search.builtin_provider.enabled` | boolean | `true` | 当活跃模型提供商支持时，默认启用提供商声明的内置网页搜索 |
| `web.search.provider` | string | `"auto"` | 默认搜索提供商，或 `auto` |
| `web.search.mode` | enum | `"fallback"` | 路由模式：`single`、`fallback` 或 `aggregate` |
| `web.search.providers` | string_list | `[]` | auto 模式下显式的提供商尝试顺序 |
| `web.search.max_results` | integer | `5` | 返回的最大结果数 |
| `web.search.max_provider_attempts` | integer | `3` | fallback/aggregate 路由尝试的最大提供商数 |
| `x_search.enabled` | boolean | `true` | 有 xAI 凭据时自动启用 `XSearch`；设为 `false` 可隐藏它 |
| `x_search.model` | model_ref | — | 用于隔离 `XSearch` 请求的可选 xAI 模型路由 |
| `x_search.timeout_seconds` | integer | `60` | 隔离 xAI `XSearch` 请求的超时 |
| `web.providers.<name>.kind` | string | required | 提供商类型：`duck_duck_go`、`searxng`、`brave`、`tencent_cloud_wsa`、`bocha`、`tavily`、`exa`、`perplexity`、`firecrawl`、`open_ai_native`、`anthropic_native`、`gemini_native` 或 `command` |
| `web.providers.<name>.base_url` | string | unset | 自定义提供商端点 |
| `web.providers.<name>.credential_profile` | string | unset | API 型提供商的凭据 profile |
| `web.providers.<name>.capabilities` | json_object | derived | `holon config get` 和路由诊断暴露的只读能力元数据 |
| `web.providers.<name>.command.argv` | string_list | unset | `kind=command` WebSearch 提供商的命令参数模板（支持 `{{query}}` 和 `{{max_results}}`） |
| `web.providers.<name>.output.format` | enum | `"json"` | 命令提供商 stdout 格式 |
| `web.providers.<name>.output.mapping.title` | string | unset | 用于映射结果标题的 JSON 路径 |
| `web.providers.<name>.output.mapping.url` | string | unset | 用于映射结果 URL 的 JSON 路径 |
| `web.providers.<name>.output.mapping.snippet` | string | unset | 用于映射结果摘要的可选 JSON 路径 |
| `web.providers.<name>.output.mapping.published_at` | string | unset | 用于映射发布时间戳的可选 JSON 路径 |
| `web.providers.<name>.limits.timeout_ms` | integer | `10000` | 命令提供商执行超时（毫秒） |
| `web.providers.<name>.limits.max_output_bytes` | integer | `200000` | 命令提供商 stdout 字节数上限 |

## 运行时数据库保留策略（Retention）

为 SQLite 运行时事件、对话记录和工具执行配置自动保留清理：

| 键 | 类型 | 默认值 | 说明 |
|-----|------|---------|-------------|
| `runtime.retention.enabled` | boolean | `false` | 启用有界运行时 SQLite 保留清理。除非显式配置，否则默认禁用。 |
| `runtime.retention.interval_hours` | positive integer | `6` | 启用保留时，daemon 执行保留清理轮次的间隔小时数。 |
| `runtime.retention.audit_events_days` | positive integer | `30` | 审计事件保留的天数窗口。 |
| `runtime.retention.audit_events_min_rows_per_scope` | positive integer | `4096` | 每个 Agent 或 host scope 独立保留的最小审计事件行数。 |
| `runtime.retention.transcript_entries_days` | positive integer | `90` | 对话记录保留的天数窗口。 |
| `runtime.retention.transcript_entries_min_rows` | positive integer | `20000` | 全局保留的最小对话记录行数。 |
| `runtime.retention.tool_executions_days` | positive integer | `90` | 工具执行保留的天数窗口。 |
| `runtime.retention.tool_executions_min_rows` | positive integer | `15000` | 全局保留的最小工具执行行数。 |
| `runtime.retention.incremental_vacuum_pages` | positive integer | `256` | 保留清理后向 SQLite 增量 vacuum 请求的最大页数。 |

## 命令任务输出安全

为后台命令任务配置磁盘输出上限、执行配额以及文件系统剩余空间水位：

| 键 | 类型 | 默认值 | 说明 |
|-----|------|---------|-------------|
| `runtime.command_task_output_retention_bytes` | integer bytes | `8388608` (8 MiB) | 单个命令任务在磁盘上保留的最大 stdout/stderr 合并字节数。超出后保留有界的头部和尾部，并插入显式截断标记。最小 4096 字节。 |
| `runtime.command_task_output_quota_bytes` | integer bytes | `67108864` (64 MiB) | 命令任务 stdout/stderr 累计输出的总字节数硬性执行配额。超出此配额将终止该命令任务以防失控。必须 $\ge$ 保留字节数。 |
| `runtime.command_task_min_free_disk_bytes` | integer bytes | `536870912` (512 MiB) | 任务产物文件系统所需保留的最小磁盘空闲字节数。低于此限制时任务终止以保护宿主磁盘。 |
| `runtime.command_task_min_free_disk_percent` | integer (0-100) | `5` | 任务产物文件系统所需保留的最小磁盘空闲百分比（0–100）。 |

### 环境变量

每个命令任务输出安全配置均可通过环境变量覆盖：

| 环境变量 | 说明 |
|----------|------|
| `HOLON_COMMAND_TASK_OUTPUT_RETENTION_BYTES` | 覆盖 `runtime.command_task_output_retention_bytes` |
| `HOLON_COMMAND_TASK_OUTPUT_QUOTA_BYTES` | 覆盖 `runtime.command_task_output_quota_bytes` |
| `HOLON_COMMAND_TASK_MIN_FREE_DISK_BYTES` | 覆盖 `runtime.command_task_min_free_disk_bytes` |
| `HOLON_COMMAND_TASK_MIN_FREE_DISK_PERCENT` | 覆盖 `runtime.command_task_min_free_disk_percent` |

## Agent 模板远程源

配置 Holon 同步以填充 Agent 模板目录的远程 Git 仓库：

| 键 | 类型 | 默认值 | 说明 |
|-----|------|---------|-------------|
| `agent_templates.remote_sources` | json_object | `{}` | 源 ID 到远程源配置的映射 |
| `agent_templates.remote_sources.<id>.url` | string | required | Git 仓库 URL（HTTPS） |
| `agent_templates.remote_sources.<id>.ref` | string | unset | Git ref（分支、标签或提交）；默认为仓库的默认分支 |
| `agent_templates.remote_sources.<id>.enabled` | boolean | `true` | 该源是否启用同步 |
| `agent_templates.remote_sources.<id>.credential_profile` | string | unset | 私有仓库的凭据 profile |

daemon 在启动时运行同步任务，把远程源模板拉取到本地模板库（`~/.agents/agent_templates`）。
重新同步会复用已记录安装映射，并拒绝覆盖本地编辑过的模板。

## HTTP 寻址

Holon 把**监听地址**和**通告地址**分开：

| 键 | 类型 | 默认值 | 说明 |
|-----|------|---------|-------------|
| `http_addr` | string | `127.0.0.1:7878` | HTTP/控制 API 的 TCP 监听地址 |
| `advertise_url` | string | unset | 向客户端通告的公网可达 URL（例如 `https://holon.example.com`） |
| `callback_base_url` | string | derived | 同主机 webhook 回调使用的本地 loopback URL |

`advertise_url` 是远程客户端（CLI、Web GUI）用来访问 daemon 的 URL。当 daemon 位于
反向代理、隧道之后，或使用的网络接口不是默认 localhost 地址时，需要设置它。

`callback_base_url` 始终由监听端口推导为 `http://127.0.0.1:<port>`，除非通过
`HOLON_CALLBACK_BASE_URL` 环境变量覆盖。这样即便 `advertise_url` 指向远程地址，同主机
webhook 回调仍可正常工作。

## Decision 子系统

Decision 子系统在 Agent 面临高歧义选择时提供非权威的咨询性第二意见（advisory second opinion），并与主对话模型解耦：

- **设计上非权威：** `AdvisoryDecision` 的结果属于结构化证据（排序选项、建议选择、置信度与推理摘要）。它绝不授予权限、不绕过执行门禁，也不改变生命周期状态。
- **提供者类型：** Holon 支持三种 Decision 提供者：
  - **本地 ONNX：** 基于 `local-onnx` feature 的嵌入式零网络外发推理，采用内置预设（如 `jev-selector-q4f16`）与 question-tail 尾部编码。
  - **原生 Jev：** 直接对接 TypeSafe Jev 协议的 HTTP 端点。
  - **OpenAI 兼容：** 显式声明 Decision 能力的标准远程模型端点。
- **路由隔离：** Decision 模型需要显式能力声明与独立路由，避免决策流量与主 Agent 对话上下文产生资源争用。
- **防御性防护：** 可配置 `decision.tools.max_calls_per_turn` 限制每轮最大调用次数，并通过 `decision.tools.min_confidence` 在置信度不足时强制弃权。

## 另请参阅

- [CLI 参考](/zh-CN/reference/cli.md)：完整的 CLI 命令参考
- [入门指南](/zh-CN/getting-started/first-agent.md)：分步设置教程
