---
title: Configuration
summary: Holon configuration files, keys, credentials, environment variables, and diagnostics.
order: 15
---

# Configuration Reference

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
| `vision.default` | model_route_ref | Route ref for ViewImage visual observation. Unset auto-discovers an image-capable provider |
| `image_generation.default` | model_route_ref | Route ref for GenerateImage requests. Unset selects the first turn model that supports image generation |
| `model.default` | model_route_ref | Default executable route, e.g. `"anthropic@default/claude-sonnet-4-6"` |
| `model.fallbacks` | model_route_ref_list | Ordered executable fallback routes |
| `runtime.disable_provider_fallback` | boolean | Disable provider/model fallback; require deterministic single-provider execution |

```bash
# Set the default model
holon config set model.default "deepseek-anthropic@default/deepseek-v4-pro"

# Add fallback models (JSON array)
holon config set model.fallbacks '["anthropic@default/claude-sonnet-4-6","minimax@default/MiniMax-M2.7"]'

# Read current default
holon config get model.default

# See all current config
holon config list

# Remove a config key (reverts to default)
holon config unset model.fallbacks
```

### Per-Model Policy

The `models.catalog` key lets you override runtime metadata for specific provider/model refs. Keys under `model.unknown_fallback.*` control policy for models without built-in metadata.

Model metadata and executable selection use distinct identities:

- `provider/model` is a logical model ref used by `models.catalog`.
- `provider@endpoint/model` is a model route ref used by defaults, fallbacks,
  vision/image generation selections, and agent overrides.

Legacy `provider/model` selection values remain accepted, but all new writes
include the endpoint. Inspect or explicitly rewrite existing config and agent
state with:

```bash
holon config migrate-model-routes          # dry-run
holon config migrate-model-routes --write  # validated canonical rewrite
```

The write creates a one-time config backup and updates all agent state in a
SQLite transaction. Invalid or ambiguous refs prevent partial writes.

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

## Credential Management

Credentials are stored securely in `~/.holon/credentials.json`. Use `config credentials` subcommands — **never edit this file directly**.

### Setting Credentials

```bash
# Preferred: use --stdin to avoid shell history leakage
holon config credentials set --kind api_key --stdin deepseek
# Paste your API key and press Enter (Ctrl+D to finish)

# Alternative: --material (visible in shell history — not recommended)
holon config credentials set --kind api_key --material "sk-..." deepseek
```

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

### Adding a Custom Provider

```bash
holon config providers set my-proxy \
  --transport openai_chat_completions \
  --base-url "https://my-proxy.example.com/v1" \
  --credential-source env \
  --credential-env "MY_PROXY_API_KEY" \
  --credential-kind api_key
```

Options:
- `--transport`: Protocol — `anthropic_messages`, `openai_chat_completions`, or `openai_responses`
- `--base-url`: API endpoint base URL
- `--credential-source`: `none`, `env`, `credential_profile`, or `external_cli`
- `--credential-kind`: `none`, `api_key`, or `session_token`
- `--credential-env`: Environment variable name (when source is `env`)
- `--credential-profile`: Credential store profile (when source is `credential_profile`)

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

If your credential expires, run `holon onboard` again — the wizard
detects the expiry and guides you through re-authentication.

### Vercel AI Gateway OIDC

Vercel AI Gateway uses OpenID Connect (OIDC) for authentication. The
onboarding wizard supports this flow when Vercel is selected as the
provider.

For both OAuth and OIDC flows, the recommended setup path is:

```bash
holon onboard
```

The wizard handles the entire OAuth flow and stores the credential
securely. You should not attempt to configure OAuth providers manually in
`config.json` unless you are scripting a headless deployment.

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

## Web Fetch/Search Settings

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `api.cors.enabled` | boolean | `true` | Enable CORS responses on the HTTP/control API; localhost/loopback origins are allowed by default |
| `api.cors.allowed_origins` | string_list | `[]` | Additional explicit browser origins allowed to call the API |
| `api.cors.allowed_methods` | string_list | `["GET","POST","PATCH","DELETE","OPTIONS"]` | HTTP methods allowed by CORS preflight |
| `api.cors.allowed_headers` | string_list | `["content-type","authorization"]` | Request headers allowed by CORS preflight |
| `api.cors.allow_credentials` | boolean | `false` | Allow credentialed CORS requests; incompatible with wildcard origins |
| `api.cors.max_age_seconds` | integer | `600` | Browser cache lifetime for preflight responses |
| `web.fetch.enabled` | boolean | `true` | Enable WebFetch tool |
| `web.fetch.max_chars` | integer | `20000` | Max characters returned to model |
| `web.fetch.max_response_bytes` | integer | `750000` | Max response bytes before truncation |
| `web.fetch.timeout_seconds` | integer | `20` | Per-request timeout |
| `web.fetch.max_redirects` | integer | `5` | Max redirect hops |
| `web.fetch.allowed_hosts` | string_list | `[]` | Hosts allowed (empty = all) |
| `web.fetch.denied_hosts` | string_list | `[]` | Hosts blocked |
| `web.search.enabled` | boolean | `true` | Enable WebSearch tool |
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

## See Also

- [CLI Reference](/reference/cli.md) — Complete CLI command reference
- [Getting Started](/getting-started/first-agent.md) — Step-by-step setup tutorial
