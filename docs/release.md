# Release

Holon releases are published from version tags.

## Versioning

Keep `Cargo.toml` aligned with the tag. For example, `v0.13.0` must be released
from a commit whose crate version is `0.13.0`.

## Publish

```bash
git tag v0.13.0
git push origin v0.13.0
```

The release workflow builds and uploads:

- `holon-linux-amd64.tar.gz`
- `holon-darwin-amd64.tar.gz`
- `holon-darwin-arm64.tar.gz`
- `checksums.txt`

The workflow also generates `Formula/holon.rb`. If `HOMEBREW_TAP_TOKEN` is
configured, it pushes the formula to `holon-run/homebrew-tap`; otherwise the
formula is available as a workflow artifact.

## Pre-Tag Checklist

Before pushing the tag, verify:

- `Cargo.toml` and `Cargo.lock` are aligned with the tag version
- release notes include a concise overview, then list notable features/fixes
  with the related feature or fix PR link on each item; do not use only the
  release-prep PR as the PR reference
- supported binary assets are Linux amd64, macOS amd64, and macOS arm64
- `checksums.txt` will be included with the release assets
- `Formula/holon.rb` will be generated, and either pushed to
  `holon-run/homebrew-tap` or retained as a workflow artifact when
  `HOMEBREW_TAP_TOKEN` is not configured
- the README quickstart uses installed `holon ...` commands rather than
  `cargo run -- ...` commands
- when provider, context projection, compaction, or prompt-cache behavior
  changed, the ignored live LLM baseline in
  `docs/testing/live-llm-baseline.md` has been run manually
- when the version number or HTTP API surface changed, regenerate the OpenAPI
  snapshot (see below) — never edit `openapi.json` by hand

## OpenAPI Snapshot

A checked-in copy of the generated OpenAPI schema lives at
`docs/website/reference/openapi.json`. The integration test
`openapi_snapshot_matches_generated_schema` verifies that this snapshot matches
the schema generated from the current crate version. Any version bump or HTTP
API change will cause drift.

**Do not edit `openapi.json` by hand.** After bumping the version in
`Cargo.toml`, regenerate the snapshot:

```bash
cargo test --test openapi_snapshot refresh_openapi_snapshot -- --ignored
cargo test --test openapi_snapshot
```

The first command regenerates the file; the second verifies it matches. The
generated output has no trailing newline — adding one manually causes the
snapshot test to fail.

The Web GUI transport types are generated from the same snapshot. Refresh both
artifacts together and verify drift with:

```bash
make transport-types
make transport-types-check
```
