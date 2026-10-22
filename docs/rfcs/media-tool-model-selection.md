---
title: RFC: Operation-scoped media selection and delivery
date: 2026-10-21
status: accepted
---

# Operation-scoped media selection and delivery

This amends [route-aware model selection](extensible-model-provider-configuration.md),
[provider capability resolution](provider-capability-registry.md), and
[portable visual observations](visual-observation-and-image-view.md).
It implements the approved outcome of issue #3621 and the accompanying image
generation parameter review.

## Selection

`ViewImage` and `GenerateImage` accept optional `model`. Canonical executable
identity is `provider@endpoint/model`; legacy `provider/model` is accepted through
the existing compatible parser. Empty input is invalid. Omission retains the
existing environment/configuration/default/automatic selection order.

An explicit value overrides the default for this invocation only. Defaults are
not model locks. No new media allowlist is required, but existing capability
policy, configured endpoints, credentials, model and transport capabilities
remain authoritative. A model string cannot supply a URL, credential, transport,
enable a provider, or mutate another invocation's configuration.

Explicit selection is strict: local rejection sends no inference request;
provider failure does not select another model or retry after dropping semantic
parameters. Existing authentication refresh and bounded same-route observation
protocol correction remain unchanged. Explicit vision calls do not advance the
automatic candidate cursor.

Results identify requested and actual canonical routes, selection source and
contract version. New fields are compatible with historical results.

## Discovery and authority

Both list tools accept `operation=view_image|generate_image`. Without a diagnostic
override, operation filtering uses operation-specific `call_selectable`, not
the text-turn `selectable` bit. An image-only model need not support agent turns.

Model `media` exposes:

- `image_input` and `image_output` (image generation), each supported,
  unsupported, or unknown;
- separate view/generation support, current callability and refusal reasons;
- a resolved generation contract when available, including semantic parameters,
  effective default behavior and native versus deliverable formats.

Model support, implemented transport lowering, credential readiness and agent
tool permission are distinct. Unknown evidence is not proof of non-support.
Provider summaries describe observed endpoint routes: supported means at least
one, callable counts are operation-specific, and incomplete inventory is
explicit. They do not claim every model supports images.

`include_unavailable` exposes diagnostics; it is not an authorization bypass.
Execution rechecks its own current route/configuration snapshot. Discovery and
execution share generation contract resolution. No second provider registry,
arbitrary JSON passthrough, or dynamic plugin mechanism is introduced.

System media guidance reports only fixed default-candidate availability and
discovery instructions for visible tools. Concrete provider/endpoint/model
identifiers, including remote catalog names, stay in discovery/tool results;
configuring a provider does not make its catalog text trusted instructions.
The system prompt and its cache identity change with candidate availability,
not candidate names. Actual routing and media result cache identity remain
route-specific and unchanged.

## Generation semantics

The stable tool schema describes the union of supported runtime inputs, not a
promise that every endpoint accepts every field. Resolve the route before
defaults and validation. The typed arguments preserve omission rather than
injecting schema defaults into provider requests.

`size` is route-constrained, not globally restricted to three old sizes.
`aspect_ratio` and `resolution` serve routes with native ratio/tier controls.
They cannot conflict with `size`. Existing xAI size mappings remain explicit
compatibility mappings, not exact pixel guarantees or nearest-size conversion.

Unsupported/unknown explicit semantic parameters fail locally with field,
route, constraints and a recovery hint. Omission uses the route's confirmed
default behavior, otherwise omits the wire field and labels an unknown provider
default honestly. In particular, explicit `background=auto` is not equivalent
to omission on an endpoint that does not accept background.

Current adapters own wire fields such as Images response_format/n, xAI native
ratio/resolution, and Codex's hosted image tool. Compatibility with an HTTP
protocol or a provider-like hostname does not establish model parameter support.

## Final file format

`output_format` is a runtime delivery requirement: png (default), jpeg or webp.
It is not a demand to forward an identically named provider field.

Prefer confirmed native output in the requested format. Otherwise use the
route's legal native output and convert locally. Detect and decode the actual
source bytes, not a MIME label or filename extension. Valid same-format output
is not re-encoded. File extension, MIME, dimensions and artifact identity refer
to actual delivered bytes; source and conversion metadata remain visible.

Conversion supports bounded single-frame PNG/JPEG/WebP only. Enforce byte,
dimension/allocation and frame limits, preserve display orientation and alpha,
and use fixed documented codec settings. No remote converter, shell command,
resize, crop or background synthesis is implicit.

Provider receipt is bounded before decoding: Images JSON is limited to 46 MiB
and hosted image-tool SSE to 96 MiB, including comments and partial events.
The SSE budget permits the same bounded image in output-item and terminal
events. Exceeding a receipt limit reports that generation may have succeeded,
without automatic regeneration.

Transparent background with JPEG fails before generation. An actually
transparent source cannot silently become opaque JPEG without a background
composition contract. Lossy JPEG encoding is disclosed; transcoding cannot
restore information already lost in a source image.

Conversion failure preserves the generated source artifact and reports
generation succeeded but delivery failed. It never repeats generation or
changes models. Replay uses the recorded source/delivery artifacts and does
not repeat paid inference.

## Cache and cost boundaries

Resolve and authorize before a ViewImage cache lookup. Cache identity includes
image/prompt/schema/generation policy, canonical route and a non-secret endpoint
configuration identity. A changed endpoint/base URL or model cannot reuse an
old observation. Cache hits retain the true original generated_by metadata.
Concurrent overrides do not mutate defaults or each other's cache identity.

Known local rejection paths have zero inference side effects. Conversion is
local, not a provider retry. Auxiliary media operations do not create a new
monetary budget system or imply unlimited spending permission; existing
admission mechanisms must be reused where applicable and any remaining quota
integration gaps must be reported separately.

## Verification

Repository regression tests cover default/explicit selection, unavailable
routes, permission filtering, endpoint isolation, same-route cache reuse,
operation discovery, semantic parameter contracts, wire lowering, source
validation/conversion and zero-request failures. Schema inventory tests check
the stable public surface. Real-model verification is explicitly authorized,
minimal, and supplements rather than replaces offline tests.
