# Model Capability Resolution

## Status

Accepted for incremental implementation.

## Problem

Holon historically stored model capability booleans next to model metadata and
used them directly for routing and UI projection. That shape conflates three
different facts:

1. what the model can intrinsically consume or produce;
2. what a provider endpoint accepts for that model;
3. what the selected Holon transport can encode and send.

The conflation is most visible for image generation and reasoning controls. A
text model may expose image generation through a hosted tool without producing
images intrinsically, and a reasoning model may not accept a user-selected
effort level on every endpoint.

## Contract

Capability resolution has three inputs:

### Intrinsic model capabilities

`ModelIntrinsicCapabilities` describes stable model properties:

- input and output modalities;
- reasoning behavior;
- model-owned execution and tool-call properties.

The canonical catalog owns this layer. Endpoint or transport facts must not be
stored here.

### Endpoint model policy

`EndpointModelPolicy` describes the contract for one model on one endpoint:

- accepted request parameters and their allowed values;
- endpoint-specific limits;
- availability and endpoint-specific restrictions.

This layer is keyed by `ModelRouteRef`, not only by `ModelRef`.

### Transport capabilities

`TransportCapabilities` describes what a Holon transport implementation can
encode. A model or endpoint declaration cannot enable behavior absent from the
transport.

### Resolved capabilities

`ResolvedModelCapabilities` is the intersection used for routing and client
projection. Routing must consume this resolved object instead of independently
checking catalog booleans and transport kinds.

`supported_parameters` remains a compatibility list of parameter names.
`parameter_contracts` is the authoritative resolved contract and carries
allowed values. Clients should migrate to the latter.

## Reasoning

Reasoning behavior and reasoning control are separate:

- `none`: the model is not declared as a reasoning model;
- `fixed`: reasoning behavior exists but has no user control;
- `effort`: the endpoint accepts named effort levels;
- `budget`: the endpoint accepts a token budget.

During migration, the legacy `supports_reasoning` flag continues to project
intrinsic `fixed` reasoning, while endpoint `reasoning_effort` options project
an accepted parameter contract. Catalog calibration will move entries to the
explicit representation without changing route identity.

## Modalities

Input and output modalities replace new uses of `image_input` and
`image_generation`. The legacy booleans remain accepted during migration and
project to `text` plus optional `image` modalities.

Hosted image generation remains an endpoint/transport behavior. It must not be
interpreted as proof that the text model intrinsically emits image bytes.

## Migration boundary

Canonical model identity and route-level metadata migration is incremental:

1. introduce and consume the resolved contract;
2. store one canonical intrinsic model entry and sparse policies keyed by
   `ModelRouteRef`;
3. migrate endpoint overrides and aliases provider by provider;
4. remove legacy booleans only after config and discovery compatibility no
   longer depend on them.

During the provider migration, legacy duplicate catalog entries may be adapted
into sparse route policies at catalog construction. Newly migrated providers
must register canonical model metadata once. Route capability values may only
narrow the canonical capability; transport capability remains the final
intersection.

## Acceptance criteria

- route selection uses one resolved capability object;
- diagnostics expose intrinsic, endpoint, transport, and resolved facts;
- parameter allowed values are available without client-side inference;
- existing config and serialized compatibility fields continue to work;
- tests cover a model capability rejected by transport and route-specific
  reasoning effort values.
