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
cargo test models_dev
```

The `models-dev-validate.yml` workflow runs these checks on PRs touching
the relevant paths.

## Safety boundaries

- Refreshing the snapshot does **not** enable providers, change routes,
  alter credentials, or modify the default model selection.
- The artifact is a CI/review intermediate; the runtime does not load it
  directly.
- Provider mapping changes require code review and merge.
