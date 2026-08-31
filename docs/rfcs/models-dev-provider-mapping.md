---
title: RFC: Explicit models.dev Provider Mapping
date: 2026-08-31
status: draft
---

# RFC: Explicit models.dev Provider Mapping

## Summary

Phase 3A adds a Holon-owned mapping manifest between `models.dev` metadata and
Holon's provider catalog. The manifest is an allowlist and validation input,
not an endpoint discovery mechanism. It makes provider, offering, route, and
model identity explicit before an upstream offering can appear in a report or
later be registered as callable.

The first protocol baseline is the direct `anthropic` provider using Holon's
Anthropic Messages transport. A later pilot may add one verified
OpenAI-compatible provider. Neither pilot changes the default route,
credential loading, transport implementation, or runtime refresh behavior.

This RFC extends [Versioned Model Registry Snapshot][snapshot-rfc] and the
Phase 2 adapter in [models.dev Upstream Adapter and Artifact Generation][adapter-rfc].

## Problem and boundary

`models.dev` is useful for discovering model offerings and metadata, but its
provider IDs, API URLs, environment-variable hints, and capability claims are
not sufficient to create a Holon route. A model name can be exposed through
multiple providers, plans, gateways, or native protocols.

The sources therefore have separate authority:

| Fact | Authoritative source |
| --- | --- |
| model metadata candidate, upstream provider/model IDs, limits and modalities | `models.dev` snapshot |
| Holon provider identity and provider kind | Holon mapping manifest |
| endpoint, plan variant, base URL policy and credential reference | Holon provider/route registration |
| transport and protocol dialect | compiled Holon transport registration |
| effective capability and limit ceiling | Holon route policy and runtime safety policy |
| default/preferred route | existing Holon catalog/configuration |

An unmapped upstream provider is reportable discovery data only. It cannot
produce a callable route.

## Four-layer identity model

The layers are related but never collapsed into a `provider/model` string:

1. **Model identity** — the canonical model concept and its intrinsic metadata.
2. **Provider identity** — the service or account namespace, such as
   `anthropic`; it is not an endpoint.
3. **Offering identity** — the provider-facing model ID and upstream
   `models.dev` offering reference.
4. **Route identity** — the Holon endpoint/plan/transport/credential combination
   that can actually be invoked.

An offering may reference a model identity without having a route. A route may
serve multiple offerings only when its registration explicitly permits those
model IDs. A model identity does not inherit capabilities from another
offering or route.

## Mapping manifest

The Phase 3A schema is versioned and Holon-owned. The following is a normative
shape; field names may be represented by Rust structs or a checked-in data
file, but the semantics must remain unchanged:

```json
{
  "schema_version": 1,
  "providers": [
    {
      "models_dev_id": "anthropic",
      "holon_provider_id": "anthropic",
      "kind": "direct",
      "transport": "anthropic_messages",
      "route_registration": "anthropic@default",
      "model_id": {
        "mode": "exact_or_pattern",
        "allow": ["claude-*"]
      },
      "capability_ceiling": {
        "tool_calling": true,
        "image_input": true,
        "image_generation": false,
        "reasoning": true,
        "structured_output": false
      },
      "limit_ceiling": {
        "context_window_tokens": 200000,
        "max_output_tokens": 8192
      },
      "credential_ref": "anthropic",
      "enabled": false,
      "provenance": {
        "owner": "holon",
        "reviewed_at": "2026-08-31"
      }
    }
  ]
}
```

Required semantics:

- `models_dev_id` is an exact upstream identity; no fuzzy matching or
  environment-variable inference is allowed.
- `holon_provider_id`, `kind`, `transport`, and `route_registration` are
  Holon-owned identities. `kind` is one of `direct`,
  `openai_compatible`, `gateway`, or `token_hub`.
- `route_registration` must resolve to an existing Holon registration. The
  manifest cannot create a base URL, credential, header, retry policy, or
  transport.
- `model_id.allow` is an explicit allowlist/pattern set. A provider mapping
  cannot authorize model IDs outside it.
- `capability_ceiling` and `limit_ceiling` are upper bounds, never claims that
  the endpoint supports every listed capability.
- `credential_ref` names an existing configuration slot only; validation must
  not read or create its secret.
- `enabled` defaults to `false` in Phase 3A. Reporting can show a validated
  candidate without making it callable.
- provenance identifies the reviewed manifest entry and must be retained in
  generated reports/artifacts.

Offering records retain both the upstream reference and the mapped identities:

```json
{
  "models_dev_ref": "anthropic/claude-sonnet-4-20250514",
  "holon_provider_id": "anthropic",
  "model_identity": "claude-sonnet-4-20250514",
  "offering_id": "anthropic/claude-sonnet-4-20250514",
  "route_registration": "anthropic@default",
  "callability": "discovery_only"
}
```

The report must not synthesize a route when `route_registration` is absent,
unknown, disabled, or incompatible with the mapped transport.

## Validation and report contract

Validation is deterministic and must produce stable, actionable diagnostics.
At minimum it rejects:

- unsupported `schema_version` or malformed identity;
- duplicate upstream provider mappings or conflicting Holon provider IDs;
- duplicate offering/model references or ambiguous allowlist matches;
- missing route registration or missing provider/transport registration;
- provider kind and transport mismatch (for example a gateway marked direct);
- provider/offering/model identity collision;
- an offering outside the explicit model allowlist;
- capability widening relative to route/runtime support;
- context or output limits above Holon policy ceilings;
- a manifest credential reference that is not a declared configuration slot.

It reports, without silently activating:

- unmapped `models.dev` providers;
- upstream metadata missing or conflicting with Holon metadata;
- a disabled but otherwise valid mapping;
- upstream API URL and environment-variable hints, as audit evidence only;
- metadata whose provenance is incomplete.

Validation output contains the manifest revision, upstream revision/hash,
entry identity, diagnostic code, severity, source values, effective value (if
one exists), and callability result. Unknown upstream fields remain unknown;
they are not converted to `false` or used to widen a capability.

The effective-value rule is conservative:

```text
effective capability = upstream assertion
                       ∩ manifest ceiling
                       ∩ route registration support
                       ∩ runtime safety policy
```

For limits, the effective value is the most restrictive applicable bound.
Conflicts retain both source values and provenance in the report.

## Pilot sequence

### 1. Anthropic Messages baseline

The first fixture maps `models_dev_id = anthropic` to the existing Anthropic
Messages transport and an existing explicit route registration. It validates
the complete identity chain while keeping `enabled = false` and preserving
the current default route. Bedrock or Vertex offerings are not included:
serving an Anthropic model does not make their native protocols Anthropic
Messages.

### 2. One OpenAI-compatible dialect

After the baseline is stable, select exactly one provider whose endpoint,
authentication, model ID handling, tool calling, streaming, and error
semantics have been verified against Holon's existing OpenAI-compatible
transport. The selection is evidence-based and does not follow provider
popularity or offering count in `models.dev`. Gateways and token hubs are
separate kinds and are not combined with this second pilot.

#### DeepSeek (`deepseek@responses`) — selected 2026-08-31

DeepSeek is the Phase 3B OpenAI-compatible baseline. The models.dev upstream
provider ID is `deepseek`; the `npm` package is `@ai-sdk/openai-compatible`,
confirming the wire protocol. Holon already has a `deepseek` provider
definition (`deepseek@default` with `AnthropicMessages` transport) and
synthesizes a derived `deepseek@responses` endpoint at runtime using
`OpenAiResponses` transport and the same `DEEPSEEK_API_KEY` credential. The
validation engine registers this derived route so that manifests referencing
`deepseek@responses` resolve without requiring a standalone
`ProviderDefinition` entry.

The first model allowlist is `deepseek-v4-pro` and `deepseek-v4-flash`.
The `deepseek-v4-flash-vision-exp` model (image input) is excluded from the
initial allowlist. `structured_output` is conservatively set to `false` in
the capability ceiling even though the upstream claims it; this produces a
warning, not an error, and can be widened in a later phase.

## Rollback and non-goals

Phase 3A produces a discovery/validation report or an explicitly disabled
artifact. Loading a bad mapping must leave the current built-in catalog
unchanged. Remote fetch, startup refresh, background refresh, credential
creation, endpoint discovery, automatic provider registration, default-route
selection, and native adapters for Bedrock/Vertex/Google/Azure are out of
scope.

[snapshot-rfc]: ./model-registry-snapshot.md
[adapter-rfc]: ./models-dev-adapter.md
