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
- Cargo manifests, lockfiles, and changes outside known ownership paths
  select the full Rust set, including shared build and CI inputs.
- Changes affecting the root package retain the existing checks and
  `lib-a`/`lib-b` compile shards. An isolated crate test change skips the
  root suite, daemon-based Web and automatic scheduler gates, and shared SDK
  job. Explicit E2E labels still work.
- PRs test selected standalone crates. Pushes check them. Domain still
  enables `error-sanitization`, and the ONNX crate remains covered by
  the root feature check.
