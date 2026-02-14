# Holon v0.11 Operator Guide

This guide is for operators running Holon in local environments or CI.

## Capability Boundary (v0.11)

| Surface | Status in v0.11 | Operator expectation |
|---|---|---|
| `holon run` | Stable (GA) | Contract-first execution kernel with deterministic artifact validation. |
| `holon solve` | Stable (GA wrapper over `run`) | End-to-end issue/PR automation with skill-driven collect/publish. |
| `holon serve` | Preview | API/ingress/control-plane behavior may change between minor releases. |

For the normative `run` contract, see `docs/run-ga-contract.md`.

## Stable Surfaces

### `holon run`

Use `holon run` when you need explicit control over goal/spec and artifact processing.

Stable operator assumptions:
- Containerized, sandboxed execution boundary.
- Output contract based on `HOLON_OUTPUT_DIR`.
- Required execution record: `manifest.json`.
- Stable skill activation precedence (`--skill/--skills`, project config, spec metadata, auto-discovered skills).

### `holon solve`

Use `holon solve` for GitHub issue/PR workflows.

Stable operator assumptions:
- Skill-first IO is the default behavior.
- `solve` orchestrates workspace preparation, agent execution, and publish validation.
- Issue and PR flows are handled by GitHub-focused skills.

Upgrade impact to keep in mind:
- Existing workflows that relied on legacy Go collector/publisher defaults should migrate to skill-driven behavior and skill configuration.

## Preview Surface

### `holon serve`

`holon serve` is preview in v0.11.

Preview caveats:
- Ingress/control-plane paths and method set can evolve.
- Webhook mode is primarily for local development/testing.
- Backward compatibility for serve-specific APIs is not guaranteed at the same level as `run`/`solve`.

Current webhook docs and caveats: `docs/serve-webhook.md`.

## Upgrade Notes (to v0.11)

1. Treat `run` and `solve` as the stable operator entrypoints.
2. Treat `serve` as preview; pin versions and validate behavior before rollout.
3. For `solve`, align automation to skill-first IO and skill outputs.
4. Ensure automation reads artifacts via env contract (`HOLON_OUTPUT_DIR`) instead of hardcoded absolute paths.

## Known Limitations and Mitigations

1. `serve` webhook mode is local-first and includes explicit caveats.
Mitigation: keep production automation centered on `run`/`solve` until serve API is finalized.

2. Skill workflows depend on GitHub credentials/tooling.
Mitigation: standardize token provisioning (`GITHUB_TOKEN`/`GH_TOKEN`) and verify auth in CI preflight.

3. Skill outputs can vary by workflow while still honoring manifest contract.
Mitigation: gate automation on `manifest.json` status/outcome and documented required metadata.

## Troubleshooting Entry Points

- Runtime and contract safety: `docs/run-safety-checklist.md`
- Skill-first architecture and behavior: `docs/modes.md`
- Manifest/output semantics: `docs/manifest-format.md`
- State mount and persistence: `docs/state-mount.md`
- Serve webhook preview operation: `docs/serve-webhook.md`
- Contributor/debug reference: `docs/development.md`

## Release Notes Mapping

- Operator-facing boundary statement for v0.11: this document.
- Changelog entries: `CHANGELOG.md`.
