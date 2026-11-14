---
title: Configure models, credentials, and fallbacks
summary: Authenticate a provider, select global or per-agent routes, and verify the route used for a test turn.
order: 130
---

# Configure models, credentials, and fallbacks

Choose a working provider connection before selecting the model for an agent.
For route identities and fallback behavior, see
[Models and providers](/concepts/models-and-providers.md).

## Before you start

- Access to the Holon installation you intend to configure.
- A provider account or a local inference server.
- Permission to change shared configuration. Global defaults and credentials
  can affect several agents.

Run these commands in your operator terminal. Do not paste credentials into
agent prompts, screenshots, logs, or issue reports.

## Steps

1. Inspect registered providers and configuration health:

   ```bash
   holon config providers list
   holon config doctor
   ```

   For interactive setup, including supported OAuth/device-login providers:

   ```bash
   holon onboard
   ```

   Follow the wizard for your provider. If using an API key instead, store it
   through standard input and connect the profile to the provider. This example
   uses the built-in `deepseek` provider:

   ```bash
   holon config credentials set --kind api_key --stdin deepseek
   ```

   Enter the key and finish input with Ctrl+D before running the next commands:

   ```bash
   holon config set providers.deepseek.auth.source credential_profile
   holon config set providers.deepseek.auth.profile deepseek
   holon config set providers.deepseek.auth.kind api_key
   ```

   A profile name is a label, not an automatic provider assignment. Alternatively,
   use the provider's documented environment variable in the daemon's environment.
   Exporting it in a separate client shell does not update a running daemon.
   Never use `--material` for a real secret in a shared command history.

2. List models after authentication:

   ```bash
   holon config models list
   ```

   Select an available agent-turn route from the output. Canonical routes use
   `provider@endpoint/model`, for example
   `deepseek@default/deepseek-v4-pro`. Availability still depends on
   your account, endpoint, and installed configuration.

3. Set a global default using your chosen route:

   ```bash
   holon config set model.default "deepseek@default/deepseek-v4-pro"
   holon config get model.default
   ```

   To select a different route for one existing agent:

   ```bash
   holon agent model set "anthropic@default/claude-sonnet-4-6" reviewer
   holon agent model get reviewer
   ```

   Use a route available to your installation; these two examples require
   different provider connections. `holon agent model clear reviewer` removes
   the override so the agent uses the global default again.

4. Choose a fallback policy:

   ```bash
   # Automatic: one preferred route per authenticated provider.
   holon config set model.fallbacks '["auto"]'

   # Or an explicit ordered list of routes you have authenticated.
   holon config set model.fallbacks '["anthropic@default/claude-sonnet-4-6"]'
   ```

   Run only the policy you intend to use. An agent override replaces the
   primary route; the global default is not implicitly added as a fallback.
   Empty or unset fallback lists use automatic fallback. To require a single
   provider route instead, set:

   ```bash
   holon config set runtime.disable_provider_fallback true
   ```

   This is global. Set it back to `false` when you want fallback restored.
   Review where prompts may be sent before enabling additional providers.

5. Read any `config set` notes on stderr. A persisted change may still require
   a daemon restart to take effect. If requested, wait for active work to
   settle and use your installation's normal daemon restart procedure.
   Then test one agent:

   ```bash
   holon config doctor
   holon run --agent reviewer "Reply with a one-sentence greeting. Do not use tools or change files."
   holon agent get reviewer --detail full
   holon transcript --agent reviewer --limit 20
   ```

## Confirm it worked

The model list and doctor report show an available primary route. The test
returns a brief. In agent detail, inspect `model.requested_model`,
`model.active_model`, and `model.fallback_active` when present, and compare
them with the transcript's provider metadata. These distinguish the configured
preference from the route used after fallback. Do not rely on the model
identifying itself in prose.

## Local or custom endpoints

For a local Ollama server, pull a model in Ollama first, then select its
available route, such as `ollama@default/<installed-model>`. No API key is
required for the built-in localhost provider.

For a custom provider, use the declared management command:

```bash
holon config providers set my-proxy \
  --transport openai_chat_completions \
  --base-url "https://my-proxy.example.com/v1" \
  --credential-source env \
  --credential-env MY_PROXY_API_KEY \
  --credential-kind api_key
```

Replace the endpoint and configure its credential in the daemon environment.
Inspect `holon config models list` before selecting it. Endpoint, plan, and
model-metadata fields are in the
[Configuration reference](/reference/configuration.md).

### Non-sensitive static request headers

Use the header-only commands to preserve an existing provider's credentials,
base URL, cache settings, plans, and other endpoints:

```bash
holon config providers headers set my-proxy \
  --header 'X-Project=example' --header 'User-Agent=my-client'
holon config providers headers set my-proxy --endpoint coding \
  --header 'X-Project=coding'
holon config providers headers unset my-proxy --endpoint coding --name X-Project
```

`--header` and `--name` are repeatable. `set` upserts keys; the first `=` separates
the name from its literal value, and an empty value is valid. Names are normalized
to lowercase; duplicate names within one command or map are rejected regardless
of case. The endpoint must already be configured or registered by the provider.
An unconfigured built-in provider keeps its built-in authentication defaults;
an unknown custom provider is an error.

Provider headers are inherited by each endpoint. Endpoint entries override
matching provider entries, and removing an endpoint entry restores inheritance.
An empty endpoint map inherits rather than clearing the provider map. Headers
apply only to that inference route, not discovery, OAuth refresh, downloads, or
other providers.

Each layer and the merged result allow at most 32 headers, 4096 bytes per value,
and 16384 bytes total across names and values. Invalid HTTP names/values and
CR/LF are rejected. Authentication, cookies, protocol negotiation, hop-by-hop,
and adapter-owned session/routing headers are reserved; use credential settings
for API keys instead. Ordinary headers such as `User-Agent` are configurable.

**Use only non-sensitive values.** Values are stored literally in `config.json`,
and command arguments may appear in shell history and process listings. There is
no secret-store or environment-variable interpolation for these headers.
Provider inspection and header-command output show names, not values; this does
not make the raw configuration file or general-purpose configuration reads a
redacted view. These commands persist changes to the local configuration store;
they do not claim to hot-reload a running daemon.

### OpenCode Go sessions

The standard `opencode-go/<model>` reference uses Messages, not native-model
protocol selection. Explicit `opencode-go@default/<model>` retains legacy Chat;
Go has no built-in Responses endpoint or alias. Existing canonical routes
are not migrated. This preference neither certifies translated capabilities
for every model nor switches protocol after a failure.

The built-in `opencode-go` provider uses `OPENCODE_GO_API_KEY`. Its `default`
and `messages` endpoints automatically receive a runtime-owned,
opaque `x-opencode-session` and a default `User-Agent: holon/<version>`.
Legacy `opencode-go-messages` references use the same
session rules.

The session identifies the Holon installation and agent incarnation, not a
single request or WorkItem. It stays stable through turns, retries, tools,
fallbacks, compaction, and daemon restarts, and changes for another installation
or a deleted-and-recreated agent. It is independent of prompt caching.
Do not configure `x-opencode-session` manually for Go: that is rejected, and
calls without trusted runtime session context fail locally before HTTP.
An unrelated custom provider may use that header name as a normal static header.

Use `holon config models list` to choose an available
`opencode-go@<endpoint>/<model>` route. A static ordinary `User-Agent` override is
allowed; neither a static header nor a session identifier replaces authentication.

## Configure image tools separately

Choose routes that support the operation and whose credentials are available
to the daemon. For example, with the corresponding providers configured:

```bash
holon config set vision.default "anthropic@default/claude-sonnet-4-6"
holon config set image_generation.default "openai@default/gpt-image-2"
holon config get vision.default
holon config get image_generation.default
```

These settings leave the conversation model unchanged. Follow any restart
notice, then test [image inspection](inspect-image.md) with a harmless local
image, or [image generation](generate-image.md) with a small request. Check the
tool's actual result and provider selection, not just the stored configuration.
Generation may incur provider charges.

Without explicit routes, the tools use their documented discovery rules.
See [vision selection](../reference/view-image.md#vision-model-selection) and
[image routing](../reference/image-generation.md#image-routing) for capability
and transport requirements. A working text conversation does not verify either
image tool.

## Troubleshooting

- **Missing or expired credentials:** rerun `holon onboard` for OAuth, or
  update the intended credential profile through stdin. Check the provider's
  configured credential source.
- **Unknown or unavailable route:** copy a route from the model list and
  check endpoint/model access. Metadata declarations do not give an account
  access to a model.
- **Unexpected provider:** inspect the agent override, fallback policy, and
  actual execution metadata; then decide whether fallback should be disabled.
- **Stored value differs from running behavior:** inspect stderr for the
  applied-via result or restart note. Do not repeatedly rewrite the same key.
- **Custom catalog edit:** `models.catalog` is an object value. Preserve
  existing entries when adding overrides instead of replacing the whole map
  with a one-entry example.

For advisory-only model routing, use
[Configure advisory decisions](/guides/configure-decision-subsystem.md).
