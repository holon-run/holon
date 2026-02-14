# Changelog

All notable changes to this project are documented in this file.

## [Unreleased]

### Documentation

- Added `docs/operator-guide-v0.11.md` to define the v0.11 operator boundary:
  - Stable: `holon run`, `holon solve`
  - Preview: `holon serve`
  - Upgrade notes, known limitations, and troubleshooting entry points
- Added top-level docs links from `README.md` and `README.zh.md` to the operator guide.

### Breaking Changes (Documented)

- `holon solve` uses skill-first IO as the default behavior.
  - Collect/publish behavior is delegated to skills.
  - Operator workflows should migrate to skill-driven collect/publish assumptions.
