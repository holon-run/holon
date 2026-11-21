# Rust CI dependency selection

PRs compare the checked-out merge commit with its first parent. Pushes compare
against the event's previous commit. If the diff or package declarations are
unavailable, Rust CI uses the full existing set of checks. Scheduled and
manual runs also use the full set.

- Changes confined to a standalone crate's `tests/`, `src/**/tests.rs`, or
  equivalent `tests` or `*_tests` modules run that crate's tests. Exported
  `test_support` modules can affect consumers and use the dependency graph.
- An implementation change selects the owning crate and all transitive
  consumers via normal and build dependencies. A dev dependency selects
  its direct consumer's tests and ends propagation through that edge.
  Optional and target-specific path dependencies are included.
- Root integration test targets at `tests/*.rs` select the root suite.
  Markdown RFCs do not expand the Rust selection.
- Cargo manifests, lockfiles, and changes outside known ownership paths
  select the full Rust set, including shared build and CI inputs.
- Changes affecting the root package retain the existing checks and
  `lib-a`/`lib-b` compile shards. An isolated crate test change skips the
  root suite, daemon-based Web and automatic scheduler gates, and shared SDK
  job. Explicit E2E labels still work.
- PRs test selected standalone crates. Pushes check them. Domain still
  enables `error-sanitization`, and the ONNX crate remains covered by
  the root feature check.

Changes confined to `src/cli.rs`, `src/tail.rs`, and their Rust submodules
build and check the application. One Rust Test job builds and runs both
application unit targets in the same cache. The CLI integration job adds
the tail and daemon CLI cases normally assigned to `misc`. The application
still builds its runtime dependency and exercises the production binary
through the existing downstream gates.

Changes confined to runtime test files with a single explicit compile
group run that group on PRs and pushes. The selector reads the existing
module cfg in source. Shared fixtures, module declarations, sources with
ambiguous ownership, and mixed scopes follow the conservative selection.
The matrix keeps the established shard names and memory budgets. Nightly
and manual runs still build and test the complete set.
