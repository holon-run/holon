---
title: RFC: Extensible Model And Provider Configuration
date: 2026-04-27
status: draft
---

# RFC: Extensible Model And Provider Configuration

## Summary

Holon should make model and provider configuration extensible by separating four
concerns:

- provider runtime definitions
- model metadata and runtime policy
- model selection policy
- agent-specific model state

The first implementation should stay small. Holon should introduce an internal
provider registry and file-backed provider/model configuration, while keeping
the current `provider/model` refs, runtime default model, fallback chain, and
agent-level model override behavior.

This RFC builds on:

- `runtime-configuration-surface.md`
- `agent-profile-model.md`
- `agent-control-plane-model.md`

## Problem

Holon's current model configuration is useful but still too tightly coupled to
the first set of providers.

Today Holon already has:

- `provider/model` references
- runtime-wide `model.default`
- runtime-wide `model.fallbacks`
- per-agent primary model override
- built-in model metadata
- per-model runtime policy overrides
- one explicit unknown-model fallback policy

That is enough for OpenAI Codex, OpenAI, and Anthropic. It is not enough for
the next provider expansion.

The main issues are:

- provider identity is still effectively a closed enum
- provider runtime fields are spread across provider-specific config structs
- model metadata overrides are not a general model catalog surface
- endpoint/auth/transport configuration is not cleanly separated from model
  selection
- adding a provider risks touching core runtime branches even when the provider
  can use an existing transport family

If Holon continues extending the current shape incrementally, `model.default`,
provider endpoint settings, auth source, model metadata, fallback behavior, and
agent override state will become one mixed configuration surface.

## Goals

- keep `provider/model` as the canonical model reference format
- allow new providers without adding a new core enum variant for every provider
- separate provider runtime configuration from model selection
- separate model metadata from provider endpoint/auth configuration
- keep agent model override as agent state, not runtime-wide config
- intentionally replace the old provider-specific config shape
- make the resolved model/provider/runtime policy inspectable
- keep the first implementation smaller than OpenClaw-style full provider
  plugins

## Non-goals

- do not design a full plugin API in this RFC
- do not require live model catalog scanning in the first implementation
- do not move secrets into `config.json`
- do not add arbitrary per-agent provider config in the first implementation
- do not change in-flight provider turns when runtime config changes
- do not make provider fallback behavior depend on hidden provider-specific
  rules that cannot be inspected

## Core Judgment

Holon should treat model configuration as a small runtime contract, not as a
provider-specific command-line convenience layer.

The durable split should be:

- `providers` defines how a provider is called
- `models` defines what a model can do and what runtime policy it needs
- `model` defines which models Holon selects by default
- agent state defines which model one agent has chosen or inherited

This keeps the lifecycle boundaries from `runtime-configuration-surface.md`:

- provider/model runtime config belongs to runtime-mutable configuration
- per-agent model override belongs to agent state
- process bootstrap values remain startup settings

## Configuration Shape

The persisted runtime config should evolve toward this shape:

```json
{
  "providers": {
    "openai-codex": {
      "transport": "openai_codex_responses",
      "base_url": "https://chatgpt.com/backend-api",
      "auth": {
        "source": "external_cli",
        "kind": "session_token",
        "external": "codex_cli"
      }
    },
    "openai": {
      "transport": "openai_responses",
      "base_url": "https://api.openai.com/v1",
      "auth": {
        "source": "env",
        "kind": "api_key",
        "env": "OPENAI_API_KEY"
      }
    },
    "anthropic": {
      "transport": "anthropic_messages",
      "base_url": "https://api.anthropic.com",
      "auth": {
        "source": "env",
        "kind": "api_key",
        "env": "ANTHROPIC_API_KEY"
      }
    }
  },
  "models": {
    "catalog": {
      "openai/gpt-5.4": {
        "display_name": "GPT-5.4",
        "context_window_tokens": 272000,
        "effective_context_window_percent": 95,
        "runtime_max_output_tokens": 8192,
        "capabilities": {
          "image_input": true,
          "reasoning_summaries": true
        }
      }
    }
  },
  "model": {
    "default": "openai/gpt-5.4",
    "fallbacks": [
      "anthropic/claude-sonnet-4-6"
    ],
    "unknown_fallback": {
      "prompt_budget_estimated_tokens": 64000,
      "runtime_max_output_tokens": 8192
    }
  }
}
```

This is the first implementation shape. The schema break is intentional:
`models.catalog` replaces the previous `model.overrides` metadata surface, and
provider entries use the new runtime definition shape directly.

## Provider Runtime Definitions

A provider definition should answer:

- which provider id owns this provider
- which transport family it uses
- which endpoint base URL it uses
- how auth is resolved
- which provider-specific runtime switches are needed by the transport family

The first provider shape should be intentionally small:

```rust
pub struct ProviderRuntimeConfig {
    pub id: ProviderId,
    pub transport: ProviderTransportKind,
    pub base_url: String,
    pub auth: ProviderAuthConfig,
}
```

`ProviderId` should become a normalized string-like id, not a closed enum that
requires a source change for every provider.

The transport kind should stay closed at first:

- `openai_codex_responses`
- `openai_responses`
- `openai_chat_completions`
- `anthropic_messages`

That lets Holon add provider ids and endpoints without pretending every new
provider needs a new transport implementation.

## Provider Auth And Credentials

Holon should separate credential lookup from credential format.

The previous `auth_source` field is removed rather than treated as a
compatibility shorthand. Provider entries use the full credential contract
directly.

The target shape is:

```rust
pub struct ProviderAuthConfig {
    pub source: CredentialSource,
    pub kind: CredentialKind,
    pub env: Option<String>,
    pub profile: Option<String>,
    pub external: Option<String>,
}

pub enum CredentialSource {
    Env,
    ExternalCli,
    AuthProfile,
    CredentialProcess,
    None,
}

pub enum CredentialKind {
    ApiKey,
    BearerToken,
    OAuth,
    SessionToken,
    AwsSdk,
    None,
}
```

`source` answers where Holon obtains a credential:

- `env`: read a named environment variable
- `external_cli`: reuse or invoke an external tool-owned auth source
- `auth_profile`: read a Holon-managed auth profile
- `credential_process`: run a configured process that returns a credential
- `none`: no credential is required

`kind` answers what kind of credential the provider runtime receives:

- `api_key`: static provider API key
- `bearer_token`: static bearer-style token
- `oauth`: refreshable OAuth credential material
- `session_token`: short-lived session or subscription credential
- `aws_sdk`: provider auth is resolved by an SDK-specific chain
- `none`: no credential is sent

This means API key is not an auth source. It is a credential kind.

Examples:

```json
{
  "auth": {
    "source": "env",
    "kind": "api_key",
    "env": "OPENAI_API_KEY"
  }
}
```

```json
{
  "auth": {
    "source": "auth_profile",
    "kind": "api_key",
    "profile": "openai:default"
  }
}
```

```json
{
  "auth": {
    "source": "auth_profile",
    "kind": "oauth",
    "profile": "openai:user@example.com"
  }
}
```

OpenClaw's useful split is similar:

- provider config records an auth mode such as API key, OAuth, token, or SDK
  auth
- auth profiles can store either API-key or OAuth credential material
- auth profile ordering is metadata and routing, not the secret itself

Holon should adopt the same conceptual separation without copying the whole
OpenClaw auth-profile rotation system in the first implementation.

The first implementation should support only the currently implemented cases:

- `env` + `api_key` for OpenAI-compatible and Anthropic API-key providers
- `external_cli` + `session_token` for OpenAI Codex

Future support for `auth_profile` should be additive. It should not require
changing `model.default`, `model.fallbacks`, or `models.catalog`.

### Auth Defaults And Auto Detection

Built-in providers may define default auth resolvers.

For example:

- `openai` may default to `env` + `api_key` + `OPENAI_API_KEY`
- `anthropic` may default to `env` + `api_key` using Holon's existing
  Anthropic token resolution behavior
- `openai-codex` may default to `external_cli` + `session_token` through the
  Codex CLI auth source

Operators should not need to write these built-in defaults into `config.json`
unless they are overriding them.

Custom providers should be stricter. A custom provider should either:

- declare an explicit `auth` block, or
- declare `auth: { "source": "none", "kind": "none" }`

Holon should not broadly scan environment variables such as `*_API_KEY` for a
custom provider. Sending a credential meant for one provider to an unrelated
endpoint is a security risk and makes provider behavior hard to inspect.

The only safe implicit default for custom providers is local no-auth:

- if the configured endpoint is local, such as `localhost`, `127.0.0.1`, `::1`,
  or a Unix socket, Holon may default to `none` + `none`
- if the configured endpoint is non-local, missing `auth` should be a
  configuration error unless the operator explicitly sets `none` + `none`

This keeps built-in provider ergonomics without turning custom provider auth
into hidden credential guessing.

## Model Metadata And Runtime Policy

Model metadata should answer:

- display name and description
- context window
- effective context percent
- compaction trigger policy
- max output policy
- tool-output truncation policy
- model capability flags
- optional transport compatibility hints

This is not the same as provider config.

For example, two providers may expose the same model family through different
endpoints, and one provider may expose many models with different context and
reasoning behavior. Model runtime policy should remain keyed by the full
`provider/model` ref.

The existing `ResolvedRuntimeModelPolicy` should remain the central resolved
object for prompt budgeting and compaction. Over time it may grow into a broader
resolved model runtime object, but the policy part should stay explicit and
inspectable.

## Model Selection Policy

Model selection should continue to use:

- `model.default`
- `model.fallbacks`

The fallback chain should remain ordered and deduplicated.

When an agent has a model override:

- the override is tried first
- runtime default and configured fallbacks remain inherited unless fallback is
  disabled
- disabling provider fallback means only the effective model is attempted

Future selection fields may include:

- `model.aliases`
- `model.allowlist`
- named `model.profiles`
- task or surface-specific defaults

Those should be added only when there is a concrete runtime use case.

## Agent Model State

Agent-level model override should stay in agent state.

The first-pass rule remains:

- one agent may override its primary model
- the override is not stored in runtime-wide config
- the override inherits runtime fallback policy
- child agents may inherit the parent override according to the existing agent
  lifecycle contract

Holon should not store provider endpoint settings or auth choices in agent state
unless a later RFC defines a per-agent provider profile model.

## Resolved Runtime Object

The runtime should be able to report a resolved object with:

- effective model ref
- model source
- provider runtime definition
- provider source
- fallback chain
- resolved model policy
- active agent override, if any
- whether provider fallback is disabled

Conceptually:

```rust
pub struct ResolvedModelRuntime {
    pub effective_model: ModelRef,
    pub model_source: AgentModelSource,
    pub provider: ProviderRuntimeConfig,
    pub provider_source: ConfigSource,
    pub fallback_chain: Vec<ModelRef>,
    pub policy: ResolvedRuntimeModelPolicy,
    pub agent_override: Option<ModelRef>,
    pub provider_fallback_disabled: bool,
}
```

This object should power status output, TUI rendering, event payloads, and
debugging. Operators should not have to reconstruct the active model behavior
from separate config files and agent state files.

## Prior Art

OpenClaw's useful boundary is:

- model selection is separate from provider catalog
- provider auth and provider runtime behavior are separate concerns
- provider-specific behavior is owned by provider registration rather than
  scattered across unrelated core logic

Holon should borrow the boundary, not the full surface area.

Hermes Agent's useful boundary is:

- provider identity can be resolved from a shared catalog plus local overlays
- user-defined providers can be layered over built-in providers
- model switching should persist provider, base URL, and API mode consistently

Holon should borrow the registry and overlay idea, but avoid storing endpoint
and transport details inside the same object that represents model selection.

## Migration

This implementation is a breaking schema refactor. Holon does not preserve the
old provider-specific runtime config fields or `model.overrides` alias.

Phase 1:

- materialize the built-in providers into the new provider registry internally
- accept `providers.<id>` entries with mandatory `transport`, `base_url`, and
  `auth`
- make `ProviderId` parse as a normalized string-like id
- accept `models.catalog` entries for model metadata and runtime policy
- add status output that reports the resolved provider runtime and model policy
- add tests for custom provider ids using existing transport families

Phase 2:

- add CLI commands for `models list`, `models status`, and fallback editing
- decide whether model aliases or allowlists are needed
- consider provider-owned normalization hooks only after at least two providers
  need them

## Compatibility Rules

Existing persisted config that uses the old provider-specific runtime shape is
not preserved. Operators should rewrite runtime config to the new shape.

Specifically:

- `model.default` remains valid
- `model.fallbacks` remains valid
- `models.catalog` is the model metadata surface
- `model.unknown_fallback` remains valid
- provider entries must use `transport`, `base_url`, and full `auth`
- `HOLON_MODEL` remains a startup/runtime override input as it works today
- existing agent model override state remains valid

Holon should reject or ignore old metadata/provider fields rather than silently
merging them into the new runtime contract.

## Inspectability

Holon should expose the resolved model/provider state through:

- control API agent status
- runtime status
- TUI model section
- provider diagnostics

The output should distinguish:

- configured model ref
- effective model ref
- provider endpoint
- provider transport
- credential source and credential kind, without secrets
- model metadata source
- fallback chain
- active agent override

## Security And Secrets

Secrets should not be stored in `config.json`.

Provider definitions may reference auth source classes such as:

- `env`
- `external_cli`
- `auth_profile`
- `credential_process`
- `none`

Provider definitions may also identify credential kind, such as `api_key`,
`bearer_token`, `oauth`, `session_token`, `aws_sdk`, or `none`.

Status output may show credential source and kind, but not raw tokens, API keys,
refresh tokens, session credentials, or derived bearer credentials.

Config should not store raw secrets. If Holon later supports
`source = "auth_profile"`, the runtime config should store only profile ids and
routing metadata; secret material should live in the auth profile store.

## First Implementation Boundary

The first implementation should deliver:

- provider registry abstraction
- built-in materialization for the existing three providers
- string-like provider ids in `ModelRef`
- custom provider entries that use existing transport families
- `auth` parsing for `env` API keys and external CLI session credentials
- built-in provider auth defaults and strict custom-provider auth validation
- resolved model/provider runtime status
- tests for config parsing, fallback chain resolution, and provider construction

It should not deliver:

- dynamic network catalog refresh
- provider plugin loading
- multi-auth profile rotation
- Holon-managed auth profile storage
- task-specific model routing
- per-agent provider profiles

Those can be added later without changing the core split defined here.

## Open Questions

- Should provider fallback errors be classified centrally first, or should that
  wait until provider-owned behavior exists?
- Should model aliases be runtime-wide only, or can an agent profile define
  aliases later?
- Should `credential_process` be a first-class source, or should external tools
  stay modeled as named `external_cli` integrations until there are multiple
  process-style providers?

## Acceptance Criteria

This RFC is implemented enough when:

- a new OpenAI-compatible provider can be added through config without adding a
  new provider enum variant
- status output shows the effective provider runtime and resolved model policy
- agent override inheritance is unchanged
- tests prove fallback order and agent override inheritance are unchanged
- model metadata overrides no longer require a provider-specific code path
- provider auth status reports credential source and kind without exposing
  secret values
