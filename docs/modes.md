# Holon Modes (current behavior)

Holon uses a single `mode` to express what kind of work should be done and how results are published. The runner passes `HOLON_MODE` to the agent and enforces workspace/publishing semantics for each mode.

## User-facing modes today

### `solve` (issue -> PR)
- Intended for GitHub Issues.
- Workspace: writable snapshot (temp clone by default; use `--workspace` to override).
- Artifacts: expects `manifest.json`; typically produces `diff.patch` and `summary.md`.
- Publish: creates or updates a branch + PR with the patch and summary.

### `pr-fix` (update an existing PR)
- Intended for GitHub PRs with review comments or CI fixes.
- Workspace: writable snapshot targeting the PR branch.
- Artifacts: `manifest.json`; optional `diff.patch`, `summary.md`, and `pr-fix.json` for replies.
- Publish: applies/pushes `diff.patch` to the PR head branch and posts replies/comments when provided.

## CLI behavior
- `holon solve <ref>` (recommended): auto-detects mode based on the reference.
  - Issue reference → `mode=solve`
  - PR reference → `mode=pr-fix`
  - You can override with `--mode`, but defaults above match the publishing flow.
- `holon run --mode <mode>`: lower-level entrypoint; default mode is `solve` if not set.
- Runner always sets `HOLON_MODE` for the agent; agents may use this to pick prompt/behavior.

## Publishing semantics by mode
- `solve`: create/update a PR from Issue context; commits and pushes the patch on a branch, then opens/updates the PR with `summary.md`.
- `pr-fix`: push fixes to the existing PR branch; optional inline replies driven by `pr-fix.json`.

## Workspace and safety
- Both modes run in Docker with a snapshot workspace by default. Use `--workspace` to point at an existing checkout; otherwise, Holon clones or re-clones to a temp dir.
- Output goes to a temp directory unless `--output` is set; artifacts are validated before publish.

## Roadmap (not yet implemented in the runner)
- `plan`: read-only planning, producing `plan.md` (and optionally structured JSON); no code writes.
- `review`: read-only code review, producing `review.json` + `summary.md`; no code writes.
- Composite flows (e.g., plan-then-solve, review-then-fix) would run as separate mode invocations to preserve RO/RW guarantees.

