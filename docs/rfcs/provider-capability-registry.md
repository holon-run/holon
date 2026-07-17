# Provider Capability Registry

Handle: `rfc-provider-capability-registry`

Status: Implemented

## Purpose

Holon supports a closed set of provider transport families and a larger set of
built-in provider endpoints. The facts required to expose those providers must
be reviewable in one static registry instead of being repeated across config,
model discovery, model catalog registration, and transport construction.

This RFC defines that registry boundary. It does not define a plugin system and
does not allow runtime code loading or dynamic provider registration.

## Two registry layers

### Transport definitions

Every `ProviderTransportKind` has exactly one static transport definition. The
definition owns:

- the stable persisted wire name;
- conservative transport capabilities;
- the provider builder used for resolved model routes.

`ProviderTransportKind` parsing, display helpers, capability queries, and
provider construction delegate to this definition. Adding a transport kind
without a definition is a contract failure.

Transport capabilities describe only lowering implemented by the transport.
They do not infer model capabilities. Unknown or unimplemented capabilities
remain false.

### Provider definitions

Every built-in legacy provider id has exactly one static provider definition.
The definition owns:

- the legacy provider id;
- the canonical provider and endpoint route;
- the transport family;
- the built-in config materializer and its static defaults;
- optional model discovery auth, route, and decoder behavior;
- the static model catalog registration policy and factory.

The canonical route is the identity used by model routing. The legacy id remains
the compatibility key used by persisted provider config and existing CLI and
control API surfaces.

## Static definitions and runtime config

The static provider definition registry is not the runtime `ProviderRegistry`.
Static definitions describe built-in defaults and supported behavior. Runtime
config is still materialized from built-in defaults, persisted config,
environment and credential stores.

Custom providers do not require a static provider definition. They may select
one of the closed transport families through persisted config. Custom providers
do not acquire discovery, catalog, or capability support merely because their
id resembles a built-in provider.

## Model discovery

A discovery definition is complete and typed. It contains:

- whether discovery requires a configured credential;
- a source URL builder;
- a response decoder;
- an explicit static catalog policy.

The shared discovery runner owns HTTP, bearer authentication, response hashing,
cache persistence, timestamps, and error wrapping. Provider decoders are pure
with respect to their explicit provider id, response bytes, and timestamp.
Decoders must not read an implicit clock.

Discovery support is present only when a definition exists. Partial collections
of provider-id matches are forbidden.

## Static model catalog

Static catalog factories are registered by provider definition. Static model
data and provider-specific tests may live in provider-owned modules, while
public catalog types, aliases, metadata precedence, resolution, and policy
application remain in the shared catalog module.

Provider families that share route ownership may share one catalog module and
factory. File layout must follow registration ownership rather than requiring
one file for every legacy alias.

Discovery-only providers declare that policy explicitly. A provider with both
static and discovered metadata must register at least one matching static
catalog entry.

## Compatibility requirements

Registry migration must preserve:

- provider and endpoint ids;
- persisted transport wire names;
- built-in base URLs and credential metadata;
- model discovery URLs, authentication, decoding, cache keys, and errors;
- transport request URLs, headers, bodies, streaming decoders, and errors;
- model aliases, preferred routes, metadata precedence, and public projections.

The registry must not add providers, transports, authentication flows, model
capabilities, or runtime plugin behavior as a side effect of consolidation.

## Verification

Contract tests must verify:

- each transport kind has one unique definition and wire name;
- each built-in legacy provider id and canonical route is registered exactly as
  intended;
- each discovery definition has auth, route, decoder, and catalog policy;
- materialized built-in config matches the definition route and transport;
- static-and-discovery providers produce matching static catalog entries;
- discovery-only providers are explicitly marked;
- custom providers can continue to reuse closed transport families.

Existing transport and discovery behavior tests remain the compatibility
evidence for wire behavior.
