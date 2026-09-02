---
title: RFC: models.dev Upstream Adapter and Artifact Generation
date: 2026-08-31
status: draft
issue:
  - 2702
---

# RFC: models.dev Upstream Adapter and Artifact Generation

## Summary

This RFC defines Phase 2A+2B of the models.dev integration: a CI-time
adapter that reads a pinned `models.dev` snapshot, projects it into Holon's
canonical model registry format, and emits an immutable artifact with
provenance metadata. The runtime never fetches `models.dev` at run time; the
reviewed supplemental catalog (`models.dev/supplemental_catalog.json`, see
[119 models.dev Supplemental Catalog](../implementation-decisions/119-models-dev-supplemental-catalog.md))
merges into the built-in catalog only through PR review and merge.

This RFC builds on the accepted [Versioned Model Registry Snapshot][snap-rfc]
and its four fact layers: `ModelDefinition`, `ProviderOffering`,
`EndpointAvailability`, and `RuntimeSupport`.

## Motivation

Phase 0/1 (GitHub #2702) established the versioned built-in snapshot. The
next step is to automate model-metadata updates from `models.dev` without
putting network access on the startup or turn hot path.

The operator confirmed the following boundary:

- CI publishes side auto-follow only; no daemon runtime auto-refresh.
- `models.dev` → Holon-format artifact → CI review/merge; no direct
  consumption of floating upstream responses.
- Endpoint ref uses canonical `provider@endpoint-variant` (e.g.
  `dashscope@token-plan`). Legacy hyphenated aliases like
  `dashscope-token-plan` are no longer supported.
- Capabilities can only narrow, never widen. Missing upstream fields
  stay `unknown`, not `false`.
- Pricing, knowledge cutoff, and documentation links are provenance
  metadata only; they do not participate in route or default selection.

## Design

### Dual-source separation

| Source | Responsibility |
--------|----------------|
| `models.dev` | Model identity, capabilities, modalities, token limits, provider offering metadata |
| Holon endpoint manifest | `base_url`, transport, credential, headers, rate limits, security policy |

The two are connected by an explicit provider mapping. The adapter cannot
guess `dashscope@token-plan` from a `models.dev` provider name alone. If no
trusted mapping exists, the model is `unbound` and does not enter a
resolvable route.

### Upstream DTO

The adapter defines an independent DTO that mirrors the `models.dev` JSON
schema. All fields are `Option<T>` to preserve tri-state semantics: a
missing field is `unknown`, not `false`. The DTO does not share types with
runtime catalog structures to avoid accidental coupling.

### Projection rules

The projection maps `models.dev` fields to Holon `BuiltInModelMetadata`:

| models.dev field | Holon field | Rule |
|------------------|-------------|------|
| provider `id` + model `id` | `model_ref` | Concatenate as `provider/model` |
| `name` | `display_name` | Direct |
| `description` | `description` | Direct |
| `limit.context` | `context_window_tokens` | Direct |
| `limit.output` | `default_max_output_tokens` and `max_output_tokens_upper_limit` | Same value |
| `modalities.input` contains `"image"` | `capabilities.image_input` | Direct |
| `modalities.output` contains `"image"` | `capabilities.image_generation` | Direct |
| `reasoning` | `capabilities.supports_reasoning` | Direct |
| `reasoning_options` type=`effort` | `reasoning_effort_options` | Map values |
| `tool_call` | (not projected) | Holon transport controls tool support |
| `cost` | (provenance only) | Not in runtime metadata |
| `knowledge` | (provenance only) | Not in runtime metadata |

Fields not listed above (e.g. `parallel_tool_calls`, `interactive_exec`,
`auto_compact_token_limit`, `effective_context_window_percent`) use Holon
conservative defaults.

### Capability narrowing

The projection cannot manufacture capabilities absent from the upstream.
If `models.dev` says `reasoning: false`, Holon's
`capabilities.supports_reasoning` is `false`. If `models.dev` omits the
field, the DTO preserves `None` (unknown), but the serialized artifact's
`ModelCapabilityFlags` uses plain `bool`, so the omitted field collapses
to conservative `false`. Phase 3 narrowing must check the DTO's
`Option<bool>` presence before AND-ing; it must not narrow built-in
capabilities based on absent evidence in the artifact.

### Provider mapping

An explicit `ProviderMapping` table connects `models.dev` provider IDs to
Holon provider IDs. The default table covers direct matches (e.g.
`anthropic`, `openai`). Unmapped providers are skipped with a warning.
Custom mappings can be supplied at projection time.

### Artifact format

The artifact wraps the projected snapshot with provenance:

```json
{
  "schema_version": 1,
  "revision": "models-dev-<upstream-revision>",
  "upstream": {
    "source": "models.dev",
    "revision": "<git-sha or tag>",
    "fetched_at": "<ISO-8601>",
    "content_sha256": "<sha256 of raw upstream>",
    "adapter_version": "<crate version>"
  },
  "models": [ ... ],
  "routes": [],
  "aliases": [],
  "preferred_models": [],
  "preferred_routes": [],
  "preferred_routes_by_model": []
}
```

`routes`, `aliases`, and `preferred_*` are empty because endpoint, route,
and default selections are Holon-controlled and not derivable from
`models.dev` metadata alone.

The artifact is a CI intermediate. After review and merge, selected model
entries are integrated into the next built-in snapshot revision. The
runtime does not load the artifact directly in this phase; the supplemental
catalog is the only reviewed channel through which models.dev metadata joins
the built-in catalog.

### Artifact validation

The projected data must pass the same `RegistrySnapshot::validate()` checks
as the built-in snapshot: unique model identities, positive token limits,
valid percentages, and no capability widening.

### Legacy compatibility

The projection does not generate legacy hyphenated aliases
(e.g. `dashscope-token-plan`). Configuration that still uses such aliases
will fail at `ModelRouteRef::parse_compatible` with an explicit error.

## Implementation

New module: `src/model_catalog/models_dev/`

- `dto.rs` — upstream DTO types with tri-state `Option<T>` fields.
- `projection.rs` — DTO → `BuiltInModelMetadata` projection with provider
  mapping.
- `artifact.rs` — artifact wrapper with provenance and SHA-256 digest.
- `mod.rs` — public API and re-exports.

Test fixtures: `tests/fixtures/models_dev/`

## Non-goals

- Runtime refresh or status commands (Phase 2C+2D).
- Daemon-side network access.
- Signature verification beyond SHA-256 content digest.
- Automatic endpoint, route, or preferred selection generation.
- Pricing or knowledge cutoff in runtime route decisions.

## Future phases

- **Phase 2C**: Local registry store, provenance/status.
- **Phase 2D**: Explicit refresh/rollback with last-known-good.
- **Phase 2E**: Optional background stale-while-revalidate.

## Open questions

None. All boundary decisions were confirmed by the operator.
