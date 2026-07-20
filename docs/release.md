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

After the GitHub release is published successfully, the same tag also builds
and publishes the Linux amd64 container image:

- `ghcr.io/holon-run/holon:<version>`
- `ghcr.io/holon-run/holon:latest`

The image runs `holon serve --listen 0.0.0.0:7878` in the foreground. A
non-loopback listener requires `HOLON_CONTROL_TOKEN`, so container deployments
must provide one. The service also validates its configured model provider at
startup, so deployments must provide `HOLON_MODEL` and the corresponding
provider credentials.

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
- `make docker-smoke` passes against the production Dockerfile
- for workspace, WorkItem, scheduling, persistence, or container-runtime
  changes, the manual real-LLM Docker cases in
  `docs/testing/docker-acceptance.md` have been run with
  `make docker-live-acceptance`, or the release notes explicitly record why
  they were skipped
- the GHCR image is currently declared as Linux amd64 only
- `Formula/holon.rb` will be generated, and either pushed to
  `holon-run/homebrew-tap` or retained as a workflow artifact when
  `HOMEBREW_TAP_TOKEN` is not configured
- the README quickstart uses installed `holon ...` commands rather than
  `cargo run -- ...` commands
- when provider, context projection, compaction, or prompt-cache behavior
  changed, the ignored live LLM baseline in
  `docs/testing/live-llm-baseline.md` has been run manually with
  `make test-live` or the relevant focused live target
- when the version number or HTTP API surface changed, regenerate the OpenAPI
  snapshot (see below) — never edit `openapi.json` by hand

## OpenAPI Snapshot

A checked-in copy of the generated OpenAPI schema lives at
`docs/website/reference/openapi.json`. The integration test
`openapi_snapshot_matches_generated_schema` verifies that this snapshot matches
the schema generated from the current crate version. Any version bump or HTTP
API change will cause drift.

**Do not edit `openapi.json` by hand.** After bumping the version in
`Cargo.toml`, refresh the checked-in Rust-generated snapshots and verify them:

```bash
make snapshots-refresh
make snapshots-check
```

The first command regenerates the CLI, OpenAPI, HTTP route, and model tool schema
snapshots; the second verifies they match. Review every generated diff before
committing. The OpenAPI output has no trailing newline — adding one manually
causes the snapshot test to fail.

The Web GUI transport types are generated from the same snapshot. Refresh both
artifacts together and verify drift with:

```bash
make transport-types
make transport-types-check
```
