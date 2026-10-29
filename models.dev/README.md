# models.dev snapshot and artifact

This directory holds the checked-in `models.dev` snapshot and the Holon
artifact generated from it.

## Files

- `snapshot.json` — raw `models.dev/api.json` output fetched by
  `holon models-dev refresh`.
- `artifact.json` — Holon canonical model metadata projected from the
  snapshot using the explicit provider mapping in
  `src/model_catalog/models_dev/projection.rs`.
- `supplemental_catalog.json` — auto-drafted catalog entries for
  already-supported providers (see below). The runtime merges it into the
  built-in catalog after review.
- `refresh-summary.md` — human-readable summary of the latest refresh:
  drafted, retained, removed, and deferred models.
- `endpoint-mappings.json` — reviewed, exact-endpoint/region associations for
  native and plan catalogs. Optional `supported_models` allowlists also enumerate
  supported compatibility routes without pinned metadata. These are audit
  inputs, not runtime registrations or capability overrides.
- `metadata-review.json` — accepted route facts and documented capacity
  unknowns. Catalog changes must review the corresponding baseline changes.

## Refresh

```bash
cargo run -- models-dev refresh
```

This fetches the live `models.dev` snapshot, regenerates the artifact,
drafts the supplemental catalog, and prints a provider mapping audit
summary. The GitHub Actions workflow `models-dev-refresh.yml` runs this
only when manually dispatched and opens a generated update PR (with the
refresh summary as the PR body) when anything under `models.dev/` changes.

Daily maintenance is performed by the `holon-model-provider` agent at
04:30 Beijing time (20:30 UTC on the previous day), using an agent-owned
persistent timer rather than a GitHub Actions schedule. The agent reviews
the generated diff and deferred candidates, checks runtime-specific
adaptations such as `openai-codex` coverage and route capabilities, and
fixes and validates necessary changes before submitting a PR. Refresh
output alone is not a reviewed catalog update. No PR is created when
there is no substantive change; merging still requires maintainer approval.

Non-default subscription routes (such as `coding-plan`, `token-plan`, and
`plan`) are not inferred from a provider's general API model list. Refresh
retains explicitly reviewed routes while their models remain in the
supplement. Check each endpoint's official model IDs, capabilities, and
token limits before manually adding a new route to
`supplemental_catalog.json`; record unresolved support rather than assuming
that the API and subscription catalogs match.

See the [endpoint-specific review](endpoint-review.md) for the current manual
decisions, evidence sources, and unresolved subscription/transport limits.

## Validate

```bash
cargo run -- models-dev validate
cargo run -- models-dev coverage --check
cargo run -- models-dev coverage --json
cargo test models_dev
```

The `models-dev-validate.yml` workflow runs these checks on PRs touching
the relevant paths.

## Adding or updating a provider/model

1. Declare canonical provider/model, exact endpoints, aliases/selectors,
   transport, and region/plan boundary. Discovery does not prove eligibility.
2. Check the generic models.dev node and applicable plan/region nodes.
   Add reviewed endpoint associations, not plan metadata to the generic
   projection. Special endpoints require independent eligibility, limits,
   cache, reasoning, and capability review.
3. Inspect `models-dev coverage --json`: raw model/route metadata, effective
   field origins, internal budgets, and upstream candidates remain separate.
   Refresh summaries include existing-field differences, not only new models.
4. Change existing builtin fields in their Rust registration, then regenerate
   the snapshot using `regenerate_builtin_snapshot`. Never bypass supplement
   collision protection to update an existing entry.
5. Update `metadata-review.json` for added/changed routes. Unknown capacities
   require owner, review date, evidence, reason, and recheck condition.
   Independently choose `unknown`, `dynamic`, or `not_applicable` per field.
   Runtime output policy is not output capacity; never invent limits.
6. Run snapshot, precedence, adapter, budget, and coverage checks offline.
   Include coverage changes and unresolved endpoint evidence in the PR.

Coverage does not read local credentials, discovery caches, or user overrides.
Use `holon config models list` for a configured route's effective policy;
offline coverage is not a live probe. See the
[coverage contract](../docs/rfcs/model-metadata-coverage.md).

The initial review baseline freezes the existing checked-in catalog and its
field origins; it does not claim that every historical capacity was freshly
verified with a vendor. The focused capacity changes and remaining evidence
gaps are recorded in [metadata evidence](metadata-evidence.md). Missing limits
remain explicit review debt, including when an upstream candidate has a value.
Dynamic selectors and image-only token limits are classified separately without
adding numeric capacities.

## Safety boundaries

- Refreshing the snapshot does **not** enable providers, change routes,
  alter credentials, or modify the default model selection.
- The artifact is a CI/review intermediate; the runtime does not load it
  directly.
- Provider mapping changes require code review and merge.
