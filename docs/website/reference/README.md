---
title: Reference
summary: Current-contract snapshots of Holon's CLI, configuration, and control-plane surfaces.
order: 40
---

# Reference

Reference pages describe Holon's current public surface as it actually behaves —
not as it is planned or promised. They are verified against the compiled
runtime (`holon --help`, `holon config schema`, route inventory) and are the
authoritative single source of truth for syntax and options.

## What is covered

- **Command-line interface:** Command tree, flags, parameters, and exit codes.
- **Configuration:** Configuration files, schema validation, and environment variables.
- **HTTP control plane:** REST endpoints, authentication tokens, and payload definitions.
- **Models & tools:** Provider catalogs, model support, and built-in tool schemas.

Tutorials and step-by-step instructions belong in [Guides](/guides/). Internal engine mechanics belong in [Runtime specs](/spec/).

> **Stability note:** The runtime is pre-1.0. CLI shapes, config keys, and HTTP
> endpoints may change without prior notice. Each reference page records the
> version it was last regenerated against where applicable. See the repository
> [RFC index](https://github.com/holon-run/holon/tree/main/docs/rfcs) for design direction and stability status.

## Hand-written pages vs generated baselines

Hand-written pages are verified against a runtime artifact and record the
version they were last checked against. Generated pages and machine-readable
baselines are refreshed from source instead of edited here.

| Page | Source of truth | Refresh |
|---|---|---|
| `cli.md` | `holon --help` | regenerate when the command tree changes |
| `configuration.md` | `holon config schema`, `holon config list` | re-verify when config keys change |
| `http-control-plane.md` | Axum route tree, OpenAPI 3.1 schema (`openapi.json`) | re-verify when routes or payloads change |
| `models.md` | generated from `src/model_catalog.rs` | `cargo run --bin holon-docgen -- models > docs/website/reference/models.md`, then `python3 docs/website/.tools/sync-generated-pages.py` |
| `*-inventory.md`, `*-inventory.json` | generated JSON baselines | `make snapshots-refresh`, then review the diff |

<!-- INDEX:START -->

- [CLI reference](./cli.md)
  Holon's command-line interface — verified against holon --help (v0.44.1).
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

- [API contract inventory](./api-contract-inventory.md)
  Post-baseline stability inventory for Holon's HTTP control-plane API parameters, responses, and Phase 2 contract work.
  <!-- mdorigin:index kind=article -->

- [Model tool schema inventory](./model-tool-schema-inventory.md)
  Versioned inventory for Holon's model-facing built-in tool schemas, result envelopes, and stability labels.
  <!-- mdorigin:index kind=article -->

- [Runtime status enum inventory](./runtime-status-enum-inventory.md)
  Machine-readable baseline for stable serialized runtime lifecycle and status enums.
  <!-- mdorigin:index kind=article -->

- [Supported Models](./models.md)
  Complete reference of all built-in models and providers supported by Holon.
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
