---
title: RFC: Model Metadata Precedence
date: 2026-07-17
status: accepted
issue:
  - 2267
---

# RFC: Model Metadata Precedence

## Summary

Holon resolves model metadata field by field through one resolver contract.
Static model metadata, route-specific metadata, remote discovery, runtime
configuration, derived defaults, and transport constraints are not whole-object
alternatives.

Built-in route metadata is sparse. The canonical model entry owns intrinsic
metadata, while an exact endpoint route stores only values that differ from or
constrain that model entry.

The resolver produces:

- one runtime policy used by provider construction and runtime context policy;
- one catalog projection used by model listings;
- winner evidence for every resolved field;
- constraint evidence when a model limit or transport capability narrows a
  selected value.

Transport capabilities are final constraints. They never masquerade as model
metadata sources.

## Why

The previous implementation selected remote discovery with
`discovered.or(route_builtin)`. One discovered object could therefore erase
valid route-specific fields that discovery did not report. Model listings,
provider construction, diagnostics, and compatibility helpers also performed
their own partial merges, so the effective precedence depended on the caller.

Route identity makes whole-object replacement especially unsafe. A canonical
`ModelRef` may have several `ModelRouteRef` values whose endpoint limits or
accepted parameters differ.

## Sources

Winner evidence uses these source classes:

- `explicit_override`: a field explicitly configured under `models.catalog`;
- `remote_discovered`: metadata reported by provider discovery;
- `route_builtin`: a sparse built-in policy for the exact `ModelRouteRef`;
- `model_builtin`: built-in metadata for the canonical `ModelRef`;
- `unknown_fallback`: the explicit unknown-model fallback;
- `runtime_default`: a runtime configuration default;
- `derived`: a value calculated from other resolved fields or a stable
  built-in formula.

The existing summary `ModelMetadataSource` remains as a compatibility
projection. Field evidence is authoritative when fields have different
sources.

## Precedence Matrix

The first present value wins. Explicit override booleans preserve both `true`
and `false`. The current discovery cache has legacy non-optional booleans, so a
discovered `false` is treated as unknown rather than as an explicit denial;
only a reported `true` overrides built-in capability metadata. Unknown
discovery data must not implicitly enable or disable a capability.

| Field class | Fields | Precedence |
| --- | --- | --- |
| Display | `display_name`, `description` | explicit override, remote discovery, sparse route override, model builtin, unknown fallback, derived |
| Intrinsic/provider-reported fact | `context_window_tokens`, intrinsic capability flags | explicit override, remote discovery, canonical model builtin, unknown fallback |
| Route/endpoint contract | `max_output_tokens_upper_limit`, `reasoning_effort_options` | explicit override where supported, route builtin, remote discovery, model builtin, unknown fallback |
| Runtime policy | effective context percent, auto-compaction limit, default output size, verbosity, tool-output truncation | explicit override, route builtin, model builtin, remote discovery, unknown fallback, runtime default |
| Derived runtime policy | prompt budget, compaction trigger, compaction retention | explicit override, unknown fallback override, derived from resolved limits, runtime default |

An endpoint capability value is a constraint rather than an intrinsic metadata
winner. Missing means inherit the canonical model. `false` explicitly disables
the capability for that endpoint. A route definition cannot set `true` to
enable a capability that the canonical model does not have.

Non-empty `reasoning_effort_options` from the selected route or discovery are
authoritative. A discovered model that explicitly reports reasoning support
with an empty option list is also authoritative: it represents fixed reasoning
without a configurable effort. Empty legacy route/model built-in lists cannot
be distinguished from omitted values, so they fall through to deterministic
route/model derivation. That derivation may itself produce an empty list and
never borrows options from another route.

## Resolution Stages

### 1. Candidate collection

The resolver receives explicit inputs only:

- canonical model identity and optional exact route;
- canonical built-in metadata;
- route-specific built-in metadata;
- discovered model metadata;
- model override and unknown-model fallback;
- runtime context and output defaults;
- optional selected transport capabilities.

It performs no discovery, configuration I/O, or provider construction.

### 2. Field selection

Each field is selected independently according to the matrix. The selected
value and its origin are recorded together. Derived fields record `derived`
rather than borrowing the source of one input.

### 3. Model constraints

Model and endpoint numeric upper limits safely clamp runtime values. Endpoint
capability restrictions similarly disable an otherwise selected intrinsic
capability. Constraint evidence records the requested and effective values and
distinguishes `endpoint_policy` from model and transport constraints. Invalid
ranges or route policies that widen intrinsic capabilities are rejected during
built-in catalog construction or configuration validation.

### 4. Transport constraints

Transport capability is intersected with the resolved model and endpoint
contract:

- unsupported image input or output resolves to unsupported;
- the model's intrinsic facts remain visible separately;
- constraint evidence identifies the transport restriction;
- an explicitly requested route capability that is unavailable is rejected.

Safe narrowing is allowed. A user intent that cannot be satisfied must fail
rather than silently select a different semantic behavior.

## Evidence Contract

Evidence is a sidecar, not a wrapper around every value:

```rust
pub struct ModelMetadataEvidence {
    pub fields: BTreeMap<ModelMetadataField, ModelMetadataOrigin>,
    pub constraints: Vec<ModelMetadataConstraint>,
}
```

The field map contains the winner for each resolved field. Constraint entries
exist only for clamping, disabling, or normalization and contain:

- affected field;
- constraint kind and source;
- requested value;
- effective value.

Candidate history is deliberately not retained. Diagnostics need the winning
decision and final restriction, not every discarded value.

## Consumer Contract

- `RuntimeModelCatalog` is the runtime entry point for route resolution.
- Provider construction consumes `ResolvedModelRoute`; it does not recalculate
  model policy or capability intersections.
- Available-model projection consumes the same field-level resolver output.
- Diagnostics consume resolved policy and capability evidence; they do not
  infer source from config presence.
- Compatibility helpers may remain only as thin delegates to
  `RuntimeModelCatalog` or the same resolver.

There is one precedence implementation even when compatibility entry points
remain.

## Compatibility

- persisted provider, model, and override configuration formats do not change;
- built-in endpoint availability no longer requires a duplicate full model
  metadata entry;
- `ResolvedRuntimeModelPolicy.source` remains available as a summary field;
- existing model and route refs retain their serialization;
- unknown capability remains conservative and is never default-enabled;
- behavior changes only where whole-object replacement or caller-specific
  merging previously discarded a valid field.

## Non-Goals

- provider discovery or capability registry design;
- context candidate or budget planner redesign;
- provider transport decomposition;
- new providers or new public configuration fields;
- retaining full candidate provenance history.

## Acceptance Criteria

- one production precedence implementation resolves every metadata field;
- remote discovery cannot erase unrelated route or model fields;
- transport constraints are applied in one final stage and remain
  distinguishable from metadata sources;
- provider construction, model listings, diagnostics, and compatibility paths
  consume the same resolved contract;
- matrix tests cover missing fields, conflicts, unknown capabilities, endpoint
  overrides, clamping, and evidence.
