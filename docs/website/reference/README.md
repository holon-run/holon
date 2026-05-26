---
title: Reference
summary: Current-contract snapshots of Holon's CLI, configuration, and control-plane surfaces.
order: 40
---

# Reference

Reference pages describe Holon's current public surface as it actually behaves —
not as it is planned or promised. They are verified against the compiled
runtime (`holon --help`, `holon config schema`) and should be refreshed when
behavior changes.

> **Stability note:** The runtime is pre-1.0. CLI shapes, config keys, and HTTP
> endpoints may change without prior notice. Each reference page records the
> version it was last regenerated against where applicable. See the repository
> [RFC index](https://github.com/holon-run/holon/tree/main/docs/rfcs) for design direction and stability status.

<!-- INDEX:START -->

- [CLI reference](./cli.md)
  Holon's current command-line interface — verified against holon --help (v0.14.1).
  <!-- mdorigin:index kind=article -->

- [CLI contract inventory](./cli-contract-inventory.md)
  First-pass stability inventory for Holon's command-line parameters, outputs, and follow-up contract work.
  <!-- mdorigin:index kind=article -->

- [CLI stability policy](./cli-stability-policy.md)
  Support policy for Holon's command-line surfaces and machine-readable output contracts.
  <!-- mdorigin:index kind=article -->

- [CLI exit codes](./cli-exit-codes.md)
  Exit-code and stream-routing contract for Holon's command-line interface.
  <!-- mdorigin:index kind=article -->

- [Configuration](./configuration.md)
  Holon configuration files, keys, credentials, environment variables, and diagnostics.
  <!-- mdorigin:index kind=article -->

- [HTTP control plane](./http-control-plane.md)
  How to think about Holon's headless integration surface.
  <!-- mdorigin:index kind=article -->

- [OpenAPI schema](./openapi.json)
  Generated baseline OpenAPI 3.1 schema for Holon's current HTTP control-plane surface.
  <!-- mdorigin:index kind=article -->

- [API contract inventory](./api-contract-inventory.md)
  Post-baseline stability inventory for Holon's HTTP control-plane API parameters, responses, and Phase 2 contract work.
  <!-- mdorigin:index kind=article -->

- [Model tool schema inventory](./model-tool-schema-inventory.md)
  Versioned inventory for Holon's model-facing built-in tool schemas, result envelopes, and stability labels.
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
