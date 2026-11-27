---
title: "Configure and Switch Models: From Local Ollama to Multi-Cloud Failover"
summary: Run agents offline with zero-token-cost local Ollama models, connect cloud providers via interactive onboarding, inspect active models conversationally, and configure resilient multi-cloud failover chains.
order: 130
---

# Configure and Switch Models: From Local Ollama to Multi-Cloud Failover

Whether you want to run models completely offline without API keys using local Ollama (ensuring absolute data privacy and zero token expense), connect top-tier cloud providers (OpenAI, Anthropic, DeepSeek), or set up resilient failover chains that keep agents working through upstream rate limits and outages, Holon's routing engine handles model management out of the box.

Holon uses a uniform `provider@endpoint/model` route format (such as `ollama@default/qwen2.5-coder:latest` or `deepseek@default/deepseek-chat`). The runtime strictly distinguishes the requested model from the active model in every turn, giving you reliable transparency in chat.

## Prerequisites

- Access to a running Holon daemon (local terminal or [Web GUI](./use-web-gui.md)).
- **For local inference**: [Ollama](https://ollama.com/) installed and running locally (defaulting to `http://localhost:11434`).
- **For cloud providers**: An active provider account and API key (DeepSeek, OpenAI, Anthropic, etc.).
- Credential hygiene: Never paste API keys into public chat prompts, issues, or screenshots. Use the onboarding wizard or environment variables.

## Steps

<a id="local-or-custom-endpoints"></a>
### Scenario 1: Zero-Config Local Inference with Ollama (Offline & Free)

To run agents entirely free and offline, local Ollama is the cleanest starting point:

1. **Pull and serve a model**:
   Download a programming or reasoning model in your terminal:
   ```bash
   ollama pull qwen2.5-coder:latest
   ```
2. **Set as the global default**:
   Setting `model.default` to an Ollama route opts local Ollama into model discovery without requiring an API key:
   ```bash
   holon config set model.default "ollama@default/qwen2.5-coder:latest"
   ```
   To browse local Ollama models before choosing a default route, run `holon config models refresh --provider ollama` or declare `providers.ollama` in `config.json`.
3. **Verify connectivity**:
   Run the diagnostics command to confirm endpoint health:
   ```bash
   holon config doctor
   ```

> **Local hardware selection & strict offline isolation**: If you run multi-turn coding agents on a 64GB desktop (such as Apple Silicon), check [our multi-turn local model benchmark](../blog/local-model-agent-benchmark.md) for measured backend and model guidance (`llama.cpp`/GGUF vs. MLX, MoE turn counts, and KV cache defaults). To lock down cloud fallback, external web access, and local toolchains completely, see [Run Private Agents in Offline and Self-Hosted Environments](./run-offline-private-agent.md).

### Scenario 2: Connect Cloud Providers via Onboarding

When you need advanced cloud reasoning models, use Holon's interactive onboarding wizard:

1. **Run the onboarding wizard**:
   ```bash
   holon onboard
   ```
   Select your provider (e.g., DeepSeek, Anthropic) from the interactive menu and paste your API key when prompted. Holon automatically creates the credential profile and links the endpoint.
2. **Or inject via environment variables**:
   Export standard keys in the environment where the Holon daemon starts:
   ```bash
   export DEEPSEEK_API_KEY="sk-..."
   export ANTHROPIC_API_KEY="sk-..."
   ```
   *Note: Environment variables must be set in the shell session that launches the Holon daemon. Exporting variables in a temporary client terminal does not alter an already running daemon.*
3. **List available model routes** (`--all` also shows unavailable routes; `--provider <id>` narrows to one provider):
   ```bash
   holon config models list
   ```
   Copy your desired canonical route and set it as default:
   ```bash
   holon config set model.default "deepseek@default/deepseek-chat"
   ```

#### GitHub Copilot (experimental)

Holon bundles a public third-party OAuth app (the opencode project's app),
so no client ID configuration is required:

```bash
holon onboard
```

Select `github-copilot` and complete the OAuth device login. To use your own
GitHub OAuth app instead, enable its device flow and set its **client ID**
(not its app ID or a first-party editor's client ID):

```bash
holon config set providers.github-copilot.auth.client_id "<your-client-id>"
```

Alternatively, set `GITHUB_COPILOT_CLIENT_ID` in the login process's environment.
Explicit configuration takes precedence over the environment variable, then
the built-in default; blank overrides are ignored. Holon stores
the resulting credential and client ID in its own `github-copilot` profile;
it does not read IDE credentials or require a client secret.

Credential profiles keep the client ID recorded at login. If you logged in
with an older Holon default app, repeat the device login to switch to the
current bundled app; token refreshes keep the recorded app.

This direct provider is experimental: successful GitHub login does not
confirm Copilot entitlement, app/integration admission, or model access.
Discover the models available to your account and select a returned
route; there is no static model list. The provider default is OpenAI Responses:
models whose catalog entry declares the `/responses` endpoint use
`github-copilot@default/<model-id>` (the same as the unqualified
`github-copilot/<model-id>`), while catalog entries that do not declare
`/responses` retain the legacy route,
`github-copilot@chat-completions/<model-id>`.
After upgrading, replace saved `github-copilot@responses/<model-id>` references
with `github-copilot/<model-id>`. For saved Chat Completions models, re-select the
discovered route or add the `@chat-completions` suffix before the slash.
Holon sends `Copilot-Integration-Id: holon`, never an editor impersonation.
GitHub controls the accepted identities, models, policies, and usage limits.
Removing a local credential does not revoke its authorization at GitHub;
revoke the app in GitHub settings when access should end.

### Scenario 3: Verify Active Models in Chat & Override Per-Agent

In multi-agent systems, you may want your primary agent running on a cost-effective daily model while assigning a heavier reasoning model to a specialized code reviewer.

#### 1. Set a Per-Agent Override

Use `holon agent model set` to assign an agent-specific route:

```bash
# Assign a high-end reasoning model to reviewer
holon agent model set "anthropic@default/claude-sonnet-4-6" reviewer

# Optional: set or clear reasoning effort and service tier alongside the route
holon agent model set "openai@default/gpt-5.4" reviewer --reasoning-effort high --service-tier fast
holon agent model set "openai@default/gpt-6.2-sol" reviewer --service-tier ultrafast
holon agent model set "openai@default/gpt-5.4" reviewer --reasoning-effort clear --service-tier clear

# Inspect the agent's effective model setting
holon agent model get reviewer

# Clear the override to inherit the global default
holon agent model clear reviewer
```

Omitting `--reasoning-effort` or `--service-tier` keeps the agent's current
override for that setting; passing `clear` removes it.

#### 2. Verify the Active Model in Chat

Do not rely on the LLM's self-reported identity in conversational prose. Ask the agent directly in Web GUI chat:

> "Report your current active model, provider, and fallback status."

The agent inspects authoritative runtime execution metadata:
- **`model.requested_model`**: The preferred route you configured.
- **`model.active_model`**: The exact route that served the current turn.
- **`model.fallback_active`**: Indicates whether an upstream failure caused the turn to fail over to a backup provider.

### Scenario 4: Configure Multi-Cloud Failover

Upstream outages, network jitter, and HTTP 429 rate limits are common in production. Holon provides resilient multi-cloud failover:

1. **Automatic Failover (Recommended)**:
   ```bash
   holon config set model.fallbacks '["auto"]'
   ```
   When the primary route fails, Holon automatically picks the best available route from your authenticated backup providers, preventing interrupted long-lived tasks.
2. **Explicit Ordered Fallback Chain**:
   To control exact failover sequence (e.g., try DeepSeek first, fall back to Claude, and backstop on local Ollama):
   ```bash
   holon config set model.fallbacks '["anthropic@default/claude-sonnet-4-6", "ollama@default/qwen2.5-coder:latest"]'
   ```
3. **Strict Deterministic Mode (Disable Fallback)**:
   For reproducible benchmarks or compliance environments where data cannot leave a designated provider:
   ```bash
   holon config set runtime.disable_provider_fallback true
   ```
   Set it back to `false` when you want failover protection restored.

### Scenario 5: Configure Dedicated Vision and Image Routes (Optional)

Text conversation routes are decoupled from image capabilities. You can configure dedicated vision and image generation routes:

```bash
# Set default image inspection model
holon config set vision.default "anthropic@default/claude-sonnet-4-6"

# Set default image generation model
holon config set image_generation.default "openai@default/dall-e-3"
```

These settings do not affect your chat model. For details, see [Inspect Images](./inspect-image.md) and [Generate Images](./generate-image.md).

### Scenario 6: Magpie (Built-In) and Self-Hosted OpenAI-Compatible Gateways

**Magpie is built in.** The built-in `magpie` entry points at `http://127.0.0.1:3425` with the Anthropic Messages transport (`POST /v1/messages`), discovers models from `GET /v1/models` with the Magpie decoder, and works without an API key. Because it permits anonymous local access, Holon does not probe its port automatically at startup—opt in by running `holon config models refresh --provider magpie`, referencing a `magpie@default/<model>` route, or declaring `providers.magpie` in `config.json`.

```bash
# Only needed when Magpie listens on a non-default port.
holon config set providers.magpie.base_url http://127.0.0.1:4325
holon config models refresh --provider magpie
```

Loopback Magpie accepts any key, so a credential is optional. The built-in
entry already binds the optional Holon-side `MAGPIE_API_KEY` (a Holon
convention, not an official Magpie environment variable) and sends no
`Authorization` header when it is unset. Set it only for usage attribution
(`Authorization: Bearer magpie-<app-id>`) or to reach a shared/remote gateway
that requires the user gateway key:

```bash
# Optional: only for attribution or a shared/remote gateway.
export MAGPIE_API_KEY=magpie-<app-id>
```

The built-in entry declares only the Anthropic Messages endpoint. To call Magpie's OpenAI Chat Completions or Gemini protocols through Holon, add `endpoints.*`/`plans.*` as shown in [Multi-protocol gateways](#multi-protocol-gateways); the OpenAI client base is `http://127.0.0.1:3425/v1`, while the Anthropic and Gemini bases use `http://127.0.0.1:3425`.

**Other self-hosted gateways** that expose the OpenAI-compatible `GET /v1/models` endpoint can participate in model discovery without hand-writing a `models.catalog` entry for every model.

1. **Register the gateway** with one base URL and a credential source:
   ```bash
   holon config providers set my-gateway \
     --transport openai_chat_completions \
     --base-url http://gateway.internal:8080 \
     --credential-source env \
     --credential-kind api_key \
     --credential-env MY_GATEWAY_API_KEY
   ```
   `--base-url` may be either the gateway root (`http://gateway.internal:8080`) or an explicit `/v1` URL (`http://gateway.internal:8080/v1`). Both resolve to `GET /v1/models`; an explicit path prefix such as `http://gateway.internal:8080/gw` resolves to `GET /gw/v1/models`. Setting a credential is optional for gateways that accept unauthenticated discovery.

   > Custom (non-built-in) `openai_chat_completions` providers derive their discovery URL from `base_url`: `{base_url}` or `{base_url}/v1` resolves to `{base_url}/v1/models`, while a path prefix `{base_url}/<prefix>` resolves to `{base_url}/<prefix>/v1/models`. A gateway that serves its model list at another path declares it explicitly with `discovery` (see [Multi-protocol gateways](#multi-protocol-gateways)). Built-in providers (Copilot, Kilo Code, LiteLLM, vLLM, Ollama, and so on) carry explicit discovery definitions and are unaffected.
2. **Trigger discovery** (discovery is never probed at startup or during a turn, so a broken gateway cannot block agents):
   ```bash
   holon config models refresh --provider my-gateway
   ```
   The `providers set` output reports the current discovery cache state (`missing`, `stale`, `fresh`, `unauthenticated`, or `unsupported`) and the exact refresh command as `next_step`.
3. **Verify the discovered models** (`--provider <id>` narrows to this gateway; `--all` includes unavailable ones):
   ```bash
   holon config models list
   ```
   Discovered models appear automatically; select one with the usual route format:
   ```bash
   holon config set model.default "my-gateway@default/<model-id>"
   ```

Discovery cache is scoped to the derived `/v1/models` URL. Changing a provider's `base_url` invalidates the cached list immediately, even within the 24-hour TTL, so the next refresh reflects the new gateway. Discovery failures (404, timeout, missing key) are reported on the `models refresh` result and in the provider's discovery status; they do not fail startup or agent turns.

`models.catalog` remains available as an optional override for routes where discovery cannot supply the model set — for example setting context window or output limits, or non-chat transports (`anthropic_messages`, `openai_responses`) whose model catalogs are not auto-discovered:
```bash
holon config set models.catalog '{"my-gateway@default/my-model":{"context_window_tokens":131072}}'
```

> Automatic discovery currently covers only the OpenAI-compatible `GET /v1/models` shape, which standard responses expose as model IDs. Vendor extensions in the response body are not parsed yet; use `models.catalog` overrides when you need explicit budgets or capabilities.

<a id="multi-protocol-gateways"></a>

#### Multi-protocol gateways

Gateways such as Magpie or LiteLLM serve one model list but several protocol
endpoints, and the OpenAI-compatible base often differs from the base of the
Anthropic or Gemini endpoint. `discovery` is therefore configurable per
provider and per endpoint (the built-in `magpie` entry uses the same keys under
`providers.magpie`):

```bash
# Provider-level default: applies to every endpoint of this provider.
holon config set providers.my-gateway.discovery.kind openai_models
holon config set providers.my-gateway.discovery.path /models

# OpenAI client base (ends with /v1) — resolves to GET /v1/models.
holon config set providers.my-gateway.transport openai_chat_completions
holon config set providers.my-gateway.base_url http://gateway.internal:8080/v1

# Anthropic client base (no /v1 prefix) with its own discovery path.
holon config set providers.my-gateway.endpoints.anthropic.transport anthropic_messages
holon config set providers.my-gateway.endpoints.anthropic.base_url http://gateway.internal:8080
holon config set providers.my-gateway.endpoints.anthropic.discovery.path /v1/models
holon config set providers.my-gateway.plans.anthropic.endpoint anthropic
```

If the gateway returns a **Magpie-shaped** list (with `display_name`/`owned_by`/`context_window`/`kind` extension fields), select the Magpie decoder so context window, modalities and max output tokens are picked up instead of the generic OpenAI-compatible result. The built-in `magpie` provider already uses this decoder by default; other gateways opt in explicitly:

```bash
holon config set providers.my-gateway.discovery.decoder magpie
```

`decoder` defaults to `openai_compatible` (unchanged behavior); `ollama` is not configurable, and unknown values or `disabled`+`decoder` are rejected by config validation.

The same declared block in `config.json`:

```json
{
  "providers": {
    "my-gateway": {
      "transport": "openai_chat_completions",
      "base_url": "http://gateway.internal:8080/v1",
      "discovery": { "kind": "openai_models", "path": "/models" },
      "endpoints": {
        "anthropic": {
          "transport": "anthropic_messages",
          "base_url": "http://gateway.internal:8080",
          "discovery": { "kind": "openai_models", "path": "/v1/models" }
        }
      },
      "plans": { "anthropic": { "endpoint": "anthropic" } }
    }
  }
}
```

Rules:

- Resolution order is fixed: endpoint `discovery` > provider `discovery` > transport/built-in default.
- `kind` is `openai_models` (OpenAI `{ "data": [ { "id": ... } ] }`) or `disabled`, which turns discovery off even for `openai_chat_completions`.
- `path` is relative to the **effective endpoint** `base_url`: `https://gw/v1` plus `/models` is `https://gw/v1/models`. Absolute URLs, `..` segments, query strings and percent-escapes are rejected when the config loads.
- Discovery requests use the endpoint's own base URL, credential and headers, not the provider-level defaults.
- The cache is scoped to `provider@endpoint`, so sibling endpoints of one gateway keep separate model lists. Changing an endpoint's base URL or discovery path invalidates only that endpoint's cached list.
- Discovery is transport-independent: an `anthropic_messages` endpoint can read an OpenAI-shaped model list. Discovery stays optional and failing; it never blocks startup or a turn.

#### Built-in internal protocol endpoints

Some built-in accounts serve more than one protocol through sibling endpoints.
Those entries stay routable for per-model protocol selection, but they are not
listed as separate providers: `github-copilot-chat-completions`,
`opencode-go-messages`, `tencent-tokenhub-messages`, and
`tencent-tokenhub-responses`. Subscription variants with their own billing
entry point (for example `dashscope-token-plan`, `volcengine-coding`,
`xiaomi-token-plan`) remain first-class providers.

Override an internal endpoint through its parent. The parent form inherits
transport, base URL, credential, and headers, so only the changed fields are
needed:

```bash
holon config set providers.tencent-tokenhub.endpoints.messages.auth.kind api_key
```

The flat `providers.<sub-id>` form keeps working for existing configuration,
but it must declare `transport`, `base_url`, and `auth` in full; prefer the
parent form. A configured internal endpoint appears in the advanced section of
the provider list and reports `exposure: "advanced"`.

Provider default protocols follow Anthropic Messages > OpenAI Responses >
OpenAI Chat Completions: `tencent-tokenhub/<model>` resolves to the `messages`
endpoint whenever the model has a Messages route, while
`tencent-tokenhub@default/<model>` still addresses Chat explicitly.

## Troubleshooting

- **Local Ollama Connection Refused**:
  Verify the Ollama daemon is running with `curl http://localhost:11434/api/tags`. If Holon runs inside a container, use `http://host.docker.internal:11434`.
- **Environment Variables Ignored**:
  Ensure keys were exported before the Holon daemon process started. Run `holon config doctor` to verify provider credential status.
- **Unexpected Provider in Turn Output**:
  Check if `fallback_active: true` is reported. This indicates your primary route timed out or returned a rate limit, triggering the failover chain. If you require single-provider execution, enable `runtime.disable_provider_fallback true`.

---

## See Also

- [Models, Providers, and Routing Concepts](/concepts/models-and-providers.md)
- [Run Private Agents in Offline and Self-Hosted Environments](./run-offline-private-agent.md)
- [Can Local LLMs Complete Multi-Turn Coding Agent Tasks?](../blog/local-model-agent-benchmark.md)
- [Use the Web GUI](./use-web-gui.md)
- [Configure the Decision Subsystem](./configure-decision-subsystem.md)
- [Run Long-Lived Tasks and Recovery](./run-long-lived-task.md)
