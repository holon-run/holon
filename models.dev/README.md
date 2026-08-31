# models.dev snapshot and artifact

This directory holds the checked-in `models.dev` snapshot and the Holon
artifact generated from it.

## Files

- `snapshot.json` — raw `models.dev/api.json` output fetched by
  `holon models-dev refresh`.
- `artifact.json` — Holon canonical model metadata projected from the
  snapshot using the explicit provider mapping in
  `src/model_catalog/models_dev/projection.rs`.

## Refresh

```bash
cargo run -- models-dev refresh
```

This fetches the live `models.dev` snapshot, regenerates the artifact, and
prints a provider mapping audit summary. The GitHub Actions workflow
`models-dev-refresh.yml` runs this weekly and opens a PR when the snapshot
or artifact changes.

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
