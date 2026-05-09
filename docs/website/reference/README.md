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
> version it was last checked against. See the [roadmap](/roadmap/) for what is
> considered stable.

<!-- INDEX:START -->

- [CLI reference](./cli.md)
  Holon's command-line interface, command tree, and common workflows.
  <!-- mdorigin:index kind=article -->

- [Configuration](./configuration.md)
  Holon configuration files, keys, credentials, environment variables, and diagnostics.
  <!-- mdorigin:index kind=article -->

- [HTTP control plane](./http-control-plane.md)
  How to think about Holon's headless integration surface.
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->
