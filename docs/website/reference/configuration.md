---
title: Configuration
summary: Holon configuration files, keys, credentials, environment variables, and diagnostics.
order: 15
---
<!-- maintenance: hand-written; verify against `holon config schema` and `holon config list` when config keys change. Last verified against v0.54.0. -->

# Configuration Reference

For setup and verification, see
[Configure models, credentials, and fallbacks](/guides/configure-models.md) and
[Configure advisory decisions](/guides/configure-decision-subsystem.md).
For route concepts, see [Models and providers](/concepts/models-and-providers.md).

Holon stores runtime configuration in JSON files under `~/.holon/`:

| File | Purpose |
|------|---------|
| `~/.holon/config.json` | Providers, model defaults, TUI, web, and runtime settings |
| `~/.holon/credentials.json` | Permission-protected credential storage (managed via `config credentials`) |

## Configuration Keys

Use `holon config get/set/unset/list` to read and write keys. When a local
daemon is running, these commands prefer the daemon runtime config API; when no
daemon is reachable, they fall back to the offline config store. `set` and
`unset` print `applied_via=daemon_api` or `applied_via=offline_store` on
stderr, while stdout remains the script-facing JSON value/status. Unsupported
daemon updates fail with the daemon-provided rejection reason. Accepted daemon
updates are persisted to `config.json`; the running daemon may continue using
its current effective config until restart/reload support is available, and the
CLI surfaces that daemon note on stderr.
Use `holon config schema` to see every available key with its type, default,
and description.

### Model & Provider Settings

| Key | Type | Description |
|-----|------|-------------|
| `vision.default` | model_route_ref_or_auto | Route ref for ViewImage visual observation. Unset auto-discovers an image-capable provider |
| `image_generation.default` | model_route_ref_or_auto | Route ref for GenerateImage requests. Unset selects the first turn model that supports image generation |
| `model.default` | model_route_ref | Default executable route, e.g. `"anthropic@default/claude-sonnet-4-6"` |
| `model.fallbacks` | model_route_ref_list_or_auto | Ordered executable fallback routes; entries may also be `auto`. Unset or empty defaults to `auto`, which expands to one preferred route per authenticated provider |
| `runtime.disable_provider_fallback` | boolean | Disable provider/model fallback; require deterministic single-provider execution |

The provider chain is the primary model (agent override, or `model.default` when
no override is set) followed by `model.fallbacks`. `model.default` never joins
the chain as an implicit fallback; when you want it as a safety net, list it in
`model.fallbacks` explicitly or rely on the `auto` default.

Commands: `holon config get <key>`, `set <key> <value>`, `unset <key>`, and
`list`. List/object values use JSON. Follow the
[model guide](/guides/configure-models.md) to choose available routes.

### Per-Model Policy

The `models.catalog` key lets you override runtime metadata for specific provider/model refs. Keys under `model.unknown_fallback.*` control policy for models without built-in metadata.

Model metadata and executable selection use distinct identities:

- `provider/model` is a logical model ref used by `models.catalog`.
- `provider@endpoint/model` is a model route ref used by defaults, fallbacks,
  vision/image generation selections, and agent overrides.

#### Model Catalog Overrides (`models.catalog`)

`models.catalog` accepts a JSON object mapping logical model refs (`provider/model`) to override objects. Common override fields include:

| Field | Type | Description |
|-------|------|-------------|
| `capabilities.agent_turn` | boolean | Whether the model can serve primary agent turns |
| `capabilities.decision` | boolean | Opts the model into serving advisory decisions |
| `capabilities.image_input` | boolean | Declares multimodal vision input capability |
| `capabilities.image_generation` | boolean | Declares image generation capability |
| `capabilities.supports_reasoning` | boolean | Enables reasoning effort options |
| `decision_protocol` | enum | Wire protocol for decision models: `jev`, `cloudflare_clef`, `openai_decisions`, or `openai_compatible` |
| `context_window_tokens` | integer | Total context window size in tokens |
| `prompt_budget_estimated_tokens` | integer | Reserved prompt budget before auto-compaction |
| `display_name` | string | Human-readable model label |

Setting `models.catalog` replaces the object value. Preserve unrelated entries
when adding an override. Capability metadata declares supported behavior; it
does not provision a provider endpoint or grant model access.

Legacy `provider/model` selection values remain accepted, but all new writes
include the endpoint. Inspect or explicitly rewrite existing config and agent
state with:

```bash
holon config migrate-model-routes          # dry-run
holon config migrate-model-routes --write  # validated canonical rewrite
```

The write creates a one-time config backup and updates all agent state in a
SQLite transaction. Invalid or ambiguous refs prevent partial writes.

### Authentication & Session Settings

Holon supports session-based authentication for browser and Web UI clients alongside bearer token authentication.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `auth.mode` | string (`"local"` \| `"oidc"`) | `"local"` | Authentication mode. `"local"` supports bearer tokens and local cookie sessions; `"oidc"` enables OpenID Connect login flows. Changes require daemon restart. |
| `auth.oidc.issuer_url` | string | unset | OIDC issuer discovery URL (e.g. `https://auth.example.com/realms/holon`) |
| `auth.oidc.client_id` | string | unset | OIDC client identifier registered with the issuer |
| `auth.oidc.client_secret_env` | string | unset | Environment variable containing the OIDC client secret |
| `auth.oidc.redirect_uri` | string | unset | Callback redirect URI (e.g. `http://localhost:7878/api/auth/oidc/callback`) |
| `auth.session.absolute_ttl_seconds` | positive_integer_or_null | unset (`null`) | Absolute session lifetime in seconds (`null` means unlimited) |
| `auth.session.idle_ttl_seconds` | positive_integer | `604800` (7d) | Inactivity timeout in seconds before a session expires; an omitted value is clamped to a shorter explicit absolute TTL |

For a step-by-step guide covering IdP setup and session audits, see [Configure OIDC authentication](/guides/configure-oidc-authentication.md).

> **Session Lifetime Rules:**
> - When configured, `auth.session.absolute_ttl_seconds` must be greater than or equal to `auth.session.idle_ttl_seconds`.
> - Setting `auth.session.absolute_ttl_seconds` to `null` (or omitting it) disables the absolute cutoff. For backwards compatibility, setting `0` normalizes to `null`.
> - Production deployments require HTTPS for both `issuer_url` and callback endpoints. HTTP is only permitted when the callback host is `localhost`.
> - Daemon restart is required when changing `auth.mode` or OIDC parameters.

```bash
# Configure session timeouts
holon config set auth.session.idle_ttl_seconds 43200

# Enable OIDC authentication
holon config set auth.mode "oidc"
holon config set auth.oidc.issuer_url "https://auth.example.com/realms/holon"
holon config set auth.oidc.client_id "holon-client"
holon config set auth.oidc.client_secret_env "HOLON_OIDC_CLIENT_SECRET"
```

### HTTP API CORS

CORS is enabled by default for localhost/loopback browser origins on any port:
`http://localhost:<port>`, `https://localhost:<port>`,
`http://127.0.0.1:<port>`, and `http://[::1]:<port>`. This lets local Web UIs
call a local or remote Holon HTTP/control API when they provide the required
`Authorization: Bearer <token>` header.

Configure `api.cors.allowed_origins` to add non-local browser origins such as a
LAN-hosted Web UI. These origins are added to the built-in localhost/loopback
allowlist. For LAN access, the API must also bind to a reachable address such
as `0.0.0.0:7878` or a specific LAN IP; a `127.0.0.1` bind is not reachable
from other devices. Set `api.cors.enabled=false` to disable CORS entirely.

```bash
holon config set api.cors.allowed_origins '["http://192.168.1.10:5173"]'
holon config set api.cors.allowed_methods '["GET","POST","PATCH","DELETE","OPTIONS"]'
holon config set api.cors.allowed_headers '["content-type","authorization"]'
holon config set api.cors.allow_credentials false
holon config set api.cors.max_age_seconds 600
```

Do not combine `api.cors.allow_credentials=true` with
`api.cors.allowed_origins=["*"]`; Holon rejects that unsafe combination.

### Cookie-authenticated unsafe requests

Cookie Origin protection is disabled by default. This keeps browser sessions
working through Tailscale Serve, HTTPS termination, reverse proxies, custom
domains, and standalone Vite proxies without deployment-specific configuration.
To opt into strict protection, set one or more exact origins:

```bash
holon config set api.csrf.trusted_origins '["https://admin.example:8443"]'
```

When the list is non-empty, unsafe requests (`POST`, `PUT`, `PATCH`, and
`DELETE`) authenticated by the browser's `holon_session` cookie must carry an
allowed browser source. Holon accepts an `Origin` matching the request `Host`
by scheme, host, and port, or an exact configured origin. If `Origin` is absent,
`Sec-Fetch-Site: same-origin` and same-origin `Referer` are accepted as
fallbacks; otherwise the request is rejected with HTTP `403`. The guard is
independent of CORS and also covers `/apps` plus browser session
exchange/pairing redemption. Bearer requests, Unix-socket trusted-local
requests, and native credential endpoints remain compatible. Trusted origins
must be explicit `http://` or `https://` origins with an optional port;
wildcard `*`, paths, queries, and forwarded proxy headers are not accepted.

### Prompt Attachments

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `api.prompt.attachment_max_bytes` | integer | `67108864` | Maximum decoded size in bytes for each image or file attachment (64 MiB) |

The prompt HTTP request body limit is derived as
`max(32 MiB, 2 × api.prompt.attachment_max_bytes)` (128 MiB by default),
allowing room for Base64 and JSON. Runtime reload applies changes to new
requests; each in-flight request keeps one snapshot for both limits.

### Projection Gate

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `api.projection.max_leaders` | integer | `16` | Max concurrent projection builds; more distinct keys get `429 projection_busy` until a leader frees up |
| `api.projection.cache_ttl_ms` | integer | `500` | Milliseconds a finished projection build is cached and reused |

### Scheduler

The canonical scheduler is always enabled. `runtime.scheduler` is no longer a
configurable key. For one minor release, an existing persisted
`runtime.scheduler=canonical` value or `HOLON_SCHEDULER=canonical` environment
value is accepted with a deprecation warning. `legacy` and all other values
fail startup. Remove the obsolete selector from deployment configuration.

Migration 40 marked the rollout tables as retired compatibility data. The
follow-up cleanup migration removes them after recovery reaches a fixed point.
They do not decide authority during startup, ordinary scheduler transactions,
or typed repair.

The follow-up cleanup migration fails closed while any canonical execution is
open, any execution work item is in flight, or any queue entry is dequeued. The
error lists the affected agent IDs. Stop Holon and run
`holon debug scheduler-recovery --agent <agent>` to report, apply the typed
recovery, and report again. This command can open the immediately preceding
schema without triggering cleanup; other current-binary commands cannot.

Holon v0.31.1 is the rollback release for deployments that still require the
legacy scheduler. Use it only with a pre-migration database backup; a database
migrated by the follow-up schema cleanup is not downgrade-compatible.

### Decision Subsystem Settings

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `decision.enabled` | boolean | `false` | Enable the optional Decision provider subsystem |
| `decision.model` | model_route_ref | unset | Shared provider model route used by Decision; must explicitly advertise Decision capability |
| `decision.local_onnx.enabled` | boolean | `false` | Enable the embedded local ONNX Decision provider |
| `decision.local_onnx.preset` | string | `jev-selector-q4f16` | Built-in local ONNX model preset name |
| `decision.local_onnx.model_dir` | string | unset | Local directory containing ONNX model files and manifest |
| `decision.local_onnx.variant` | string | `q4f16` | Model quantization or variant identifier |
| `decision.local_onnx.num_threads` | integer | `1` | CPU thread count for local ONNX inference |
| `decision.local_onnx.checksum` | string | unset | Optional SHA-256 verification digest for the model directory |
| `decision.timeout_ms` | integer | unset | Provider deadline in milliseconds |
| `decision.max_tokens` | integer | unset | Maximum output tokens for Decision responses |
| `decision.concurrency` | integer | unset | Maximum concurrent Decision provider requests |
| `decision.queue_capacity` | integer | unset | Maximum queued Decision provider requests |
| `decision.tools.enabled` | boolean | `false` | Expose the advisory Decision tool (`AdvisoryDecision`) to agents |
| `decision.tools.max_calls_per_turn` | integer | unset | Maximum advisory tool calls allowed in one turn (unset means unlimited) |
| `decision.tools.timeout_ms` | integer | unset | Advisory tool timeout in milliseconds |
| `decision.tools.min_confidence` | float | unset | Minimum confidence threshold (0.0 to 1.0); decisions below this threshold abstain |

An enabled local ONNX provider and a non-empty `decision.model` are mutually
exclusive. The built-in Jev route is `typesafe@default/jev-latest`.
Selecting a local preset does not download its files. See
[Configure advisory decisions](/guides/configure-decision-subsystem.md)
for provisioning, call limits, and verification.

## Credential Management

Credentials are stored securely in `~/.holon/credentials.json`. Use `config credentials` subcommands — **never edit this file directly**.

### Setting Credentials

`holon config credentials set --kind <kind> --stdin <profile>` reads secret
material from standard input. `--material <value>` is also accepted, but can
expose the secret in shell history and process arguments; use `--stdin`.
See the [credential setup steps](/guides/configure-models.md#steps).

The `<PROFILE>` argument is a label you choose (e.g. `deepseek`, `bigmodel`, `openai`).

### Listing & Removing

```bash
holon config credentials list
holon config credentials remove deepseek
```

### Environment Variables

As an alternative to the credential store, Holon reads API keys from environment variables:

| Provider | Environment Variable |
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
| NEAR AI Cloud (TEE inference) | `NEARAI_API_KEY` |
| Volcengine | `VOLCENGINE_API_KEY` or `ARK_API_KEY` |
| StepFun | `STEPFUN_API_KEY` |
| Qwen | `QWEN_API_KEY` or `DASHSCOPE_API_KEY` |
| HuggingFace | `HUGGINGFACE_API_KEY` or `HF_TOKEN` |
| Venice | `VENICE_API_KEY` |
| Chutes | `CHUTES_API_KEY` |
| NVIDIA | `NVIDIA_API_KEY` |

For a complete, up-to-date list, run `holon config providers list`.

### Credential sources

| Source | Description |
|--------|-------------|
| `none` | No credential required (local-only providers) |
| `env` | Read credential from an environment variable |
| `credential_profile` | Read credential from `~/.holon/credentials.json` by profile name |
| `external_cli` | Run an external CLI (e.g., `codex`) to obtain a credential |

## Provider Configuration

Holon ships with built-in provider definitions for 40+ providers. You can add or override providers in `config.json`.

### Listing Registered Providers

```bash
holon config providers list
```

Each provider entry shows its transport protocol (`anthropic_messages`, `openai_chat_completions`, etc.), base URL, and credential requirement.

### Ollama (local)

Holon includes a built-in [`ollama`](./models.md) provider for running models
locally with [Ollama](https://ollama.com). It requires no API key and targets
a local Ollama server at `http://127.0.0.1:11434` over the Anthropic Messages
transport.

The route format is `ollama@default/<installed-model>`. The model must already
be available on that Ollama server. See the
[local endpoint setup](/guides/configure-models.md#local-or-custom-endpoints).

The Web GUI discovers locally running Ollama models without any credential
configuration, and `ViewImage` automatically discovers Ollama vision models
for image analysis.

### Adding a Custom Provider

`holon config providers set <provider> [options]` creates or updates a provider.

Options:
- `--transport`: Protocol — `anthropic_messages`, `openai_chat_completions`, or `openai_responses`
- `--base-url`: API endpoint base URL
- `--credential-source`: `none`, `env`, `credential_profile`, or `external_cli`
- `--credential-kind`: `none`, `api_key`, or `session_token`
- `--credential-env`: Environment variable name (when source is `env`)
- `--credential-profile`: Credential store profile (when source is `credential_profile`)
- `--credential-external`: External credential command (when source is `external_cli`)

### Provider Endpoints and Plans

Provider configuration separates a provider account from its concrete endpoints.
The legacy provider keys still configure the default endpoint:

```bash
holon config set providers.openai.transport openai_responses
holon config set providers.openai.base_url "https://api.openai.com/v1"
```

These are shortcuts for `providers.openai.endpoints.default.transport` and
`providers.openai.endpoints.default.base_url`. Use endpoint keys when the same
provider account needs another transport, base URL, or credential policy:

```bash
holon config set providers.volcengine.endpoints.image-openai.transport openai_chat_completions
holon config set providers.volcengine.endpoints.image-openai.base_url "https://ark.cn-beijing.volces.com/api/plan/v3"
holon config set providers.volcengine.plans.image-openai.endpoint image-openai
```

A plan maps a stable provider alias such as `volcengine-image-openai` onto a
named endpoint. Canonical selections persist that route explicitly, for example
`volcengine@image-openai/model-id`. Existing built-in aliases and older
`provider/model` references continue to work as compatible inputs.

### Removing a Provider

```bash
holon config providers remove my-proxy
```

## Provider OAuth and login flows

Some providers use OAuth or browser-based login instead of static API keys.
Holon supports two OAuth-style flows:

### OpenAI Codex OAuth

Codex supports two OAuth flows:

- **Device OAuth (recommended)** — Holon requests a device code, prints a
  verification URL and user code. Open the URL in any browser, enter the
  code, and the daemon automatically polls for completion. The entire flow
  runs as a background job (see Web GUI Job Monitoring).
- **Browser OAuth** — The `codex` CLI opens a browser for OAuth login and
  stores the credential locally. Holon reads it via `external_cli`.

The provider uses `credential_source: external_cli` and
`credential_kind: oauth` for browser OAuth, or device OAuth via the
onboarding wizard.

Use `holon onboard` for setup or expired-credential recovery; see the
[model setup guide](/guides/configure-models.md).

### Vercel AI Gateway OIDC

Vercel AI Gateway uses OpenID Connect (OIDC) for authentication. The
onboarding wizard supports this flow when Vercel is selected as the
provider.

`holon onboard` handles supported OAuth/OIDC flows and credential storage.
Manual JSON edits are not the normal login path.

## Listing Available Models

```bash
holon config models list
```

This shows each model's availability, credential status, provider, transport, and policy (context window, max output tokens, capabilities).

## Agent-Level Model Overrides

Each agent can override the default model:

```bash
holon agent model set "anthropic@default/claude-sonnet-4-6" reviewer
```

The override is stored in the agent's own configuration, not the global `model.default`.

## Diagnostics

```bash
# Full system health check including model availability
holon config doctor

# List all configuration keys with types and defaults
holon config schema
```

`config doctor` reports: default model, fallback models, per-model availability, provider settings, and retry policy.

## Configuration File Location

Holon resolves its configuration directory as follows:

1. `$HOLON_HOME/config.json` (if `HOLON_HOME` is set)
2. `~/.holon/config.json` (fallback)

Credentials follow the same pattern with `credentials.json`.

## TUI Settings

| Key | Values | Default | Description |
|-----|--------|---------|-------------|
| `tui.alternate_screen` | `auto`, `always`, `never` | `auto` | Alternate screen buffer behavior |

TUI debug instrumentation is controlled by environment variables:

| Environment variable | Values | Default | Description |
|----------------------|--------|---------|-------------|
| `HOLON_TUI_PRESENTATION_LOG` | `1`, `true`, `yes`, `on`, `debug` | unset | Enable `<HOLON_HOME>/logs/tui/presentation.jsonl` debug logging for stream-driven presentation decisions |
| `HOLON_TUI_PRESENTATION_LOG_MAX_BYTES` | positive integer bytes | `5242880` | Rotate the presentation debug log when it reaches this size |

## Runtime Observability

OTLP trace export is optional and disabled by default. OpenMetrics is exposed
through the protected `/api/control/runtime/metrics` endpoint whenever the
control API is running.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `runtime.observability.otlp.enabled` | boolean | `false` | Enable the bounded OTLP/HTTP JSON trace exporter |
| `runtime.observability.otlp.endpoint` | string | unset | Full HTTP or HTTPS OTLP trace endpoint, commonly ending in `/v1/traces` |
| `runtime.observability.otlp.headers` | json_object | `{}` | Static non-secret request headers |
| `runtime.observability.otlp.credential_profile` | string | unset | Credential profile materialized as a bearer authorization header |
| `runtime.observability.otlp.queue_capacity` | positive integer | `1024` | Maximum spans waiting in the non-blocking exporter queue |
| `runtime.observability.otlp.batch_size` | positive integer | `128` | Maximum spans in one export request |
| `runtime.observability.otlp.batch_interval_ms` | positive integer | `1000` | Maximum batching delay |
| `runtime.observability.otlp.timeout_ms` | positive integer | `5000` | Per-request export timeout |

OTLP settings take effect when the daemon starts. See the
[runtime observability guide](/reference/observability) for Collector,
Prometheus, Grafana, alerting, and troubleshooting examples.

## Web Fetch/Search Settings

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `web.fetch.enabled` | boolean | `true` | Enable WebFetch tool |
| `web.fetch.max_chars` | integer | `20000` | Max characters returned to model |
| `web.fetch.max_response_bytes` | integer | `750000` | Max response bytes before truncation |
| `web.fetch.timeout_seconds` | integer | `20` | Per-request timeout |
| `web.fetch.max_redirects` | integer | `5` | Max redirect hops |
| `web.fetch.allowed_hosts` | string_list | `[]` | Hosts allowed (empty = all) |
| `web.fetch.denied_hosts` | string_list | `[]` | Hosts blocked |
| `web.search.enabled` | boolean | `true` | Enable WebSearch tool |
| `web.search.builtin_provider.enabled` | boolean | `true` | Enable provider-declared builtin web search by default when active provider supports it |
| `web.search.provider` | string | `"auto"` | Default search provider or `auto` |
| `web.search.mode` | enum | `"fallback"` | Routing mode: `single`, `fallback`, or `aggregate` |
| `web.search.providers` | string_list | `[]` | Explicit auto-mode provider attempt order |
| `web.search.max_results` | integer | `5` | Max results returned |
| `web.search.max_provider_attempts` | integer | `3` | Max providers attempted by fallback/aggregate routing |
| `x_search.enabled` | boolean | `true` | Enable `XSearch` automatically when xAI credentials are available; set `false` to hide it |
| `x_search.model` | model_ref | — | Optional xAI model route for isolated `XSearch` requests |
| `x_search.timeout_seconds` | integer | `60` | Timeout for isolated xAI `XSearch` requests |
| `web.providers.<name>.kind` | string | required | Provider kind: `duck_duck_go`, `searxng`, `brave`, `tencent_cloud_wsa`, `bocha`, `tavily`, `exa`, `perplexity`, `firecrawl`, `open_ai_native`, `anthropic_native`, `gemini_native`, or `command` |
| `web.providers.<name>.base_url` | string | unset | Custom provider endpoint |
| `web.providers.<name>.credential_profile` | string | unset | Credential profile for API-backed providers |
| `web.providers.<name>.capabilities` | json_object | derived | Read-only capability metadata surfaced by `holon config get` and routing diagnostics |
| `web.providers.<name>.command.argv` | string_list | unset | Command argv template for `kind=command` WebSearch providers (supports `{{query}}` and `{{max_results}}`) |
| `web.providers.<name>.output.format` | enum | `"json"` | Command provider stdout format |
| `web.providers.<name>.output.mapping.title` | string | unset | JSON path to map result title |
| `web.providers.<name>.output.mapping.url` | string | unset | JSON path to map result URL |
| `web.providers.<name>.output.mapping.snippet` | string | unset | JSON path to map result snippet |
| `web.providers.<name>.output.mapping.published_at` | string | unset | Optional JSON path to map result publication timestamp |
| `web.providers.<name>.limits.timeout_ms` | integer | `10000` | Command provider execution timeout in milliseconds |
| `web.providers.<name>.limits.max_output_bytes` | integer | `200000` | Command provider stdout byte limit |

## Runtime Database Retention

Configure automatic retention cleanup for SQLite runtime events, transcripts, and tool executions:

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `runtime.retention.enabled` | boolean | `false` | Enable bounded runtime SQLite retention. Disabled unless explicitly configured. |
| `runtime.retention.interval_hours` | positive integer | `6` | Hours between daemon retention passes while retention is enabled. |
| `runtime.retention.audit_events_days` | positive integer | `30` | Age window in days for audit event retention. |
| `runtime.retention.audit_events_min_rows_per_scope` | positive integer | `4096` | Minimum audit event rows retained independently for each agent or host scope. |
| `runtime.retention.transcript_entries_days` | positive integer | `90` | Age window in days for transcript entry retention. |
| `runtime.retention.transcript_entries_min_rows` | positive integer | `20000` | Minimum transcript rows retained globally. |
| `runtime.retention.tool_executions_days` | positive integer | `90` | Age window in days for tool execution retention. |
| `runtime.retention.tool_executions_min_rows` | positive integer | `15000` | Minimum tool execution rows retained globally. |
| `runtime.retention.incremental_vacuum_pages` | positive integer | `256` | Maximum pages requested from SQLite incremental vacuum after a retention pass. |

## Command Task Output Safety

Configure on-disk output limits, execution quotas, and filesystem free-space
waterlines for background command tasks:

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `runtime.command_task_output_retention_bytes` | integer bytes | `8388608` (8 MiB) | Maximum combined stdout/stderr bytes retained on disk per command task. Overflow preserves a bounded head and tail with an explicit truncation marker. Minimum 4096 bytes. |
| `runtime.command_task_output_quota_bytes` | integer bytes | `67108864` (64 MiB) | Hard execution quota for total emitted bytes across stdout and stderr. Exceeding this quota terminates the command task with an execution failure. Must be $\ge$ retention limit. |
| `runtime.command_task_min_free_disk_bytes` | integer bytes | `536870912` (512 MiB) | Minimum required free disk space on the task artifact filesystem. If free space drops below this limit, the task terminates to protect the host. |
| `runtime.command_task_min_free_disk_percent` | integer (0-100) | `5` | Minimum free disk space percentage (0–100) on the task artifact filesystem. |

### Environment Variables

Each command task setting can also be overridden via environment variables:

| Variable | Description |
|----------|-------------|
| `HOLON_COMMAND_TASK_OUTPUT_RETENTION_BYTES` | Override `runtime.command_task_output_retention_bytes` |
| `HOLON_COMMAND_TASK_OUTPUT_QUOTA_BYTES` | Override `runtime.command_task_output_quota_bytes` |
| `HOLON_COMMAND_TASK_MIN_FREE_DISK_BYTES` | Override `runtime.command_task_min_free_disk_bytes` |
| `HOLON_COMMAND_TASK_MIN_FREE_DISK_PERCENT` | Override `runtime.command_task_min_free_disk_percent` |

## Agent Template Remote Sources

Configure remote Git repositories that Holon syncs to populate the agent
template catalog:

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `agent_templates.remote_sources` | json_object | `{}` | Map of source IDs to remote source configs |
| `agent_templates.remote_sources.<id>.url` | string | required | Git repository URL (HTTPS) |
| `agent_templates.remote_sources.<id>.ref` | string | unset | Git ref (branch, tag, or commit); defaults to repository default branch |
| `agent_templates.remote_sources.<id>.enabled` | boolean | `true` | Whether this source is enabled for sync |
| `agent_templates.remote_sources.<id>.credential_profile` | string | unset | Credential profile for private repositories |

The daemon runs a sync job at startup to fetch remote source templates into
the local template library (`~/.agents/agent_templates`). Re-syncs reuse
recorded install mappings and refuse to overwrite locally-edited templates.

## HTTP Addressing

Holon separates the **listen address** from the **advertised address**:

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `http_addr` | string | `127.0.0.1:7878` | TCP listen address for the HTTP/control API |
| `advertise_url` | string | unset | Publicly reachable URL advertised to clients (e.g., `https://holon.example.com`) |
| `callback_base_url` | string | derived | Local loopback URL for same-host webhook callbacks |

`advertise_url` is the URL remote clients (CLI, Web GUI) use to reach the
daemon. Set it when the daemon is behind a reverse proxy, tunnel, or on a
different network interface than the default localhost address.

`callback_base_url` is always derived from the listen port as
`http://127.0.0.1:<port>` unless overridden via the
`HOLON_CALLBACK_BASE_URL` environment variable. This keeps same-host
webhook callbacks working even when `advertise_url` is set to a remote
address.

## Decision Subsystem

See [Models and providers](/concepts/models-and-providers.md) for the advisory
boundary, [Decision Subsystem Settings](#decision-subsystem-settings) for
keys, and [Configure advisory decisions](/guides/configure-decision-subsystem.md)
for local ONNX or remote setup. Recommendations never grant permission,
bypass execution gates, or alter lifecycle state.

## Supervised Subagent Maintenance

The daemon's existing deletion coordinator also observes retained private
subagents. Parent agents can call `DeleteAgent` directly; creating a child needs
no retention duration. `GetAgent` on the current parent includes a bounded
`subagent_cleanup` responsibility list without activating any child.

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `runtime.reclamation.reminders_enabled` | boolean | `true` | Deliver durable, deduplicated internal reminders; at most one batch of 16 children per parent per day |
| `runtime.reclamation.idle_retirement_enabled` | boolean | `false` | Cooperatively exit fully idle child runtime instances, preserving identity, history and files |
| `runtime.reclamation.idle_grace_seconds` | positive_integer | `600` | Observation grace before a reminder or idle instance retirement; does not expire identities |

Reminders are enabled by default; idle retirement is opt-in. Both actions use runtime config reload. Stopped or
budget-exhausted parents are deferred. Missing supervision and repeatedly failed
parent cleanup produce deduplicated operator attention without an extra model
call. Disabling an action stops new admissions; accepted deletion jobs continue.
Idle retirement rejects active work, waits, timers and pending wake obligations.
The durable default callback endpoint survives and reloads the agent through
host ingress. Validate residency and reload latency on the deployment before
enabling automatic retirement.

## Authentication and API Security

Holon enforces session lifetimes and optional CSRF protection for HTTP endpoints:

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `auth.session.idle_ttl_seconds` | positive_integer | `604800` (7 days) | Idle session lifetime before re-authentication is required |
| `auth.session.absolute_ttl_seconds` | positive_integer or null | `null` | Absolute session lifetime; null means sessions only expire on idle timeout |
| `api.csrf.trusted_origins` | string_list | `[]` | Allowed HTTP origins for cookie-authenticated mutating requests. Empty disables the Origin guard; same-origin is always trusted; wildcards are rejected |

### Session Inactivity Policy

Authenticated Web GUI and native client sessions expire after 7 days of inactivity by default. Any valid request refreshes the idle timer up to the optional `auth.session.absolute_ttl_seconds` ceiling.

### Cookie Origin Guard

When your deployment serves cookie-authenticated sessions across origins, populate `api.csrf.trusted_origins` with exact trusted origins (e.g. `["https://app.example.com"]`). Unsafe mutating requests (`POST`, `PUT`, `DELETE`, `PATCH`) with session cookies must present an exact matching `Origin` header. Requests without an Origin or with an unlisted origin fail closed with HTTP 403.

## See Also

- [CLI Reference](/reference/cli.md) — Complete CLI command reference
- [Getting Started](/getting-started/first-agent.md) — Step-by-step setup tutorial
- [Agent Templates](/reference/agent-templates.md) — Catalog of built-in agent roles
