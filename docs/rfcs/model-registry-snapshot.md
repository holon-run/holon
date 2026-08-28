---
title: RFC: Versioned Model Registry Snapshot
date: 2026-08-28
status: accepted
issue:
  - 2702
---

# RFC: Versioned Model Registry Snapshot

## Summary

Holon will separate model metadata from runtime transport behavior. The runtime
will embed a versioned, validated model registry snapshot and later accept a
Holon-published `models.dev` overlay without making startup or turn execution
depend on the network.

This RFC covers the contract and the first migration step. It does not add
remote refresh.

## Current Structure

`BuiltInModelCatalog` currently builds model metadata from Rust provider
modules. The resulting catalog mixes four different facts:

1. properties intrinsic to a model;
2. a provider's offering and route;
3. availability observed from one endpoint and credential;
4. capabilities implemented by Holon's transport.

The resolver already applies local overrides, route constraints, transport
constraints, and conservative fallbacks. The migration must preserve that
behavior while making the built-in data independently versioned and
validated.

## Decisions

### 1. Keep four fact layers separate

- `ModelDefinition` describes canonical model identity, modalities, token
  limits, and intrinsic capabilities.
- `ProviderOffering` binds a provider-facing model ID to a model definition and
  may conservatively narrow it.
- `EndpointAvailability` records what one configured endpoint and credential
  currently expose.
- `RuntimeSupport` describes what Holon's compiled transport can encode,
  decode, and enforce.

A model is usable only where these layers intersect. Registry data can never
create a transport, endpoint, credential, header, executable, or authentication
method.

The v1 built-in snapshot preserves the current catalog representation:
canonical models, route policies, aliases, and explicit preferred routes.
Later adapters may use richer source schemas, but they must project into these
separate facts before resolution.

### 2. Apply field-level precedence

Resolved fields use this precedence:

1. explicit user or project `ModelRuntimeOverride`;
2. trusted fields observed from the configured endpoint;
3. a validated Holon registry last-known-good artifact;
4. the built-in snapshot;
5. an explicit unknown or conservative runtime fallback.

An entire model object is not replaced as one unit. Each resolved field keeps
its provenance. Route and transport constraints run after metadata selection
and may only narrow the result.

### 3. Preserve unknown

Missing metadata means `unknown`, not `unsupported`. Existing public boolean
fields may migrate incrementally, but adapters and future snapshot versions
must not convert absent upstream data to `false`.

The v1 snapshot is a lossless representation of the current built-in catalog,
whose booleans are already explicit assertions. A remote adapter must use
tri-state input before projecting only validated assertions into runtime
metadata.

### 4. Treat registry data as untrusted input

Every snapshot has an explicit schema version and immutable revision. Loading
strictly parses the snapshot-owned envelope, route, alias, and preferred
selection objects, rejecting unknown fields there. Model metadata and
capability value objects are shared with discovery caches and runtime
configuration, so they retain their existing forward-compatible
deserialization behavior; the snapshot loader instead validates their
schema-defined identities, limits, options, and route constraints after
parsing. Loading also rejects unsupported schema versions, duplicate
identities, dangling aliases, invalid preferred routes, invalid limits, and
route policies that expand intrinsic boolean capability.

The embedded snapshot is build-time data, so validation failure is a developer
error and fails fast. A future remote snapshot must instead retain the previous
last-known-good artifact and report the rejected revision.

### 5. Keep startup and turns offline

The embedded snapshot is always available. Startup and turn execution never
wait for registry network access. Future refresh will run outside the turn hot
path with stale-while-revalidate behavior, bounded downloads, atomic
replacement, and rollback to last-known-good.

Endpoint discovery keeps its existing cache and lifecycle. A global registry
cache must be stored separately because “known model” and “available through
this credential” are different facts.

### 6. Use one general upstream initially

`models.dev` is the only general community upstream for the first remote
adapter. Holon will pin an upstream revision, validate and adapt it in CI, then
publish an immutable Holon artifact. Clients will not consume a floating
`models.dev` response directly.

LiteLLM may be used as a CI audit signal. Provider-specific model APIs describe
only their own offerings and endpoint availability; they do not override other
providers.

### 7. Keep defaults release-controlled

Remote registry updates cannot change preferred or default models in the first
version. Defaults affect behavior and cost and remain controlled by a Holon
release or explicit user configuration.

## Snapshot v1

The checked-in JSON artifact contains:

- `schema_version` and `revision`;
- canonical `models`;
- endpoint-specific `routes` and narrowing policies;
- compatibility `aliases`;
- explicit preferred model and route selections.

Arrays are used instead of JSON maps so duplicate identities cannot be silently
overwritten during catalog construction. Snapshot-owned objects reject unknown
fields. Shared model metadata and capability objects intentionally keep their
existing forward-compatible deserialization contract; adding strictness there
would also change discovery-cache and runtime-configuration behavior.

The runtime parses and validates the artifact before constructing the existing
`BuiltInModelCatalog` indexes. Public catalog and resolution APIs remain
unchanged.

## Validation

Snapshot validation requires:

- supported schema version and non-empty revision;
- unique model, route, alias, and preferred-selection identities;
- aliases target canonical models and do not form chains;
- routes reference an existing canonical model;
- route boolean capability policy only narrows intrinsic model capability;
- percentages are in range and token limits are positive;
- default output limits do not exceed upper limits;
- reasoning options are unique;
- preferred models and routes exist and agree with their keys.

CI also compares the generated snapshot projection with the legacy Rust
catalog during the migration. This equivalence test protects default selection,
legacy aliases, and route behavior from accidental drift.

Route numeric values are endpoint-specific facts rather than intrinsic
capability assertions. They may therefore be higher or lower than canonical
model values; precedence resolution still applies the route value only for that
endpoint. The narrowing rule applies to boolean capabilities, where a route may
disable support but cannot manufacture support absent from the canonical model.

## Follow-up

A separate phase will add the `models.dev` adapter, immutable artifact
publication, registry cache, refresh/status commands, provenance revision
reporting, and last-known-good rollback tests. Provider discovery improvements
remain independent work.
