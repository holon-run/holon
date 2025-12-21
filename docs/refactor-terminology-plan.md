# Terminology Refactor Plan (Public Release)

This document turns the terminology work into a concrete, incremental task list. It assumes:
- We keep existing behavior working (no breaking changes before public release).
- We introduce `--agent` as the new primary CLI flag, while keeping legacy flags as aliases.

Related:
- `docs/terminology.md` (final terms + mapping)
- `docs/modes.md` (mode/profile design)

## Phase 0 — Freeze Terms (decision only)
- [ ] Confirm public terms: Runner / Agent / Engine / Mode / Role / Outputs / Publisher
- [ ] Confirm role set (MVP): `developer`, `reviewer`
- [ ] Confirm mapping & deprecations:
  - “adapter” → “agent”
  - “adapter image” → “agent bundle” (current implementation: `.tar.gz` bundle)
  - “host/runtime” → “runner”

**Acceptance**
- `docs/terminology.md` matches team understanding and becomes the single reference.

## Phase 1 — Docs-first Rename (no code changes)
- [ ] Update docs/RFC wording to prefer “agent/engine/runner” (keep legacy words only in a migration note).
- [ ] Add a small conceptual diagram showing: Runner → Agent → Engine → Outputs → Publisher.
- [ ] Ensure README uses the new terms and links to the terminology page.

**Acceptance**
- New users can understand the architecture by reading README + terminology only.

## Phase 2 — CLI & Action Compatibility Layer (public-facing API)

### CLI (`holon run`)
- [ ] Add `--agent-bundle` flag:
  - default: local agent bundle (built from `images/adapter-claude` when available)
  - help text uses “agent bundle”
- [ ] Update log output and errors to use the new terms:
  - “agent” instead of “adapter”
  - “runner” instead of “host/runtime”

**Files**
- `cmd/holon/main.go`
- `cmd/holon/runner.go`
- tests: `cmd/holon/runner_test.go`

**Acceptance**
- `holon run --agent-bundle <bundle.tar.gz> ...` works.

### Environment variables
- [ ] Add new env var name(s) to mirror `--agent` (e.g. `HOLON_AGENT`).
- [ ] Keep legacy env var(s) as aliases (if they exist) and document precedence.

**Acceptance**
- Precedence is documented and covered by unit tests.

### GitHub Action (`action.yml`)
- [ ] Add a new input `agent` (optional) and prefer it over older names.
- [ ] Keep current behavior as default; print a one-line migration hint in logs.

**Files**
- `action.yml`
- `.github/workflows/holon-issue.yml` (only if needed for examples)

**Acceptance**
- Existing workflows continue to work without changes.
- A workflow can set `with: agent: ...` to override.

## Phase 3 — Internal Renames (optional, later)
- [ ] Rename internal structs/fields to match public terms:
  - `AdapterImage` → `Agent` (or `AgentRef`)
  - `Adapter` wording in logs/messages → `Agent`
- [ ] Keep package names stable until churn is acceptable (e.g. `pkg/runtime/docker` can stay).

**Acceptance**
- Internal naming is consistent, but external compatibility remains intact.

## Phase 4 — Agent Bundle Resolver (enables npm/binary later)
- [ ] Define a bundle reference format for `--agent` (initially keep it simple):
  - default: treat `--agent` as a docker image name
  - future: prefixes like `docker:...`, `npm:...`, `file:...`
- [ ] Implement a resolver interface:
  - `docker` resolver (current behavior)
  - `npm` resolver: design stub / behind a feature flag

**Acceptance**
- No behavior change for docker users.
- Code structure allows adding an npm-based agent bundle without redesigning flags.

## Phase 5 — Publishers (boundary first, features later)
- [ ] Keep “publisher” as the external layer (workflows/scripts) that consumes outputs.
- [ ] Document the MVP publishers we rely on today:
  - Patch Publisher: apply `diff.patch` + commit + PR update
  - Summary Publisher: post `summary.md` to step summary / PR body
  - Review Publisher (future): publish `review.json` as PR review

**Acceptance**
- Clear boundary: agents generate outputs; publishers apply them to GitHub.
