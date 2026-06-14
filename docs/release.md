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
- release notes include a concise overview and links to the relevant merged PRs
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
