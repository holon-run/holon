# Terminology Refactor Plan (Public Release)

This document turns the terminology work into a concrete, incremental task list. It assumes:
- We keep existing behavior working (no breaking changes before public release).
- We introduce `--agent` as the new primary CLI flag, while keeping legacy flags as aliases (e.g. `--agent-bundle`).

Related:
- `docs/terminology.md` (final terms + mapping)
- `docs/modes.md` (mode/profile design)
- `docs/agent-bundle.md` (bundle format)
- `rfc/0002-agent-scheme.md` (normative agent contract)

## Current Status (as implemented)
- `agents/claude/` contains the Claude agent implementation and build scripts.
- `holon run` accepts `--agent` (primary) and `--agent-bundle` (deprecated alias).
- `HOLON_AGENT` is supported (with `HOLON_AGENT_BUNDLE` as legacy alias).

## Phase 0 — Freeze Terms (decision only)
- [x] Confirm public terms: Runner / Agent / Engine / Mode / Role / Outputs / Publisher
- [x] Confirm role set (MVP): `developer`, `reviewer`
- [x] Confirm mapping & deprecations:
  - “adapter” → “agent”
  - “adapter image” → “agent bundle” (current implementation: `.tar.gz` bundle)
  - “host/runtime” → “runner”

**Acceptance**
- `docs/terminology.md` matches team understanding and becomes the single reference.

## Phase 1 — Docs-first Rename (no code changes)
- [x] Update docs/RFC wording to prefer “agent/engine/runner” (keep legacy words only in a migration note).
- [x] Add a small conceptual diagram showing: Runner → Agent → Engine → Outputs → Publisher.
- [x] Ensure README uses the new terms and links to the terminology page.

**Acceptance**
- New users can understand the architecture by reading README + terminology only.

## Phase 2 — CLI & Action Compatibility Layer (public-facing API)

### CLI (`holon run`)
- [x] Add `--agent` flag (new primary):
  - accepts an agent reference (MVP: a local `.tar.gz` bundle path)
  - default behavior: auto-detect the latest bundle under `agents/claude/dist/agent-bundles/` when present
- [x] Keep `--agent-bundle` as an alias (deprecated):
  - still works
  - help text marks it as deprecated (or hide from help)
- [x] Update log output and errors to use the new terms:
  - “agent” instead of “adapter”
  - “runner” instead of “host/runtime”
- [x] Update `--role` help text/examples to match the public role set:
  - MVP: `developer`, `reviewer`

**Files**
- `cmd/holon/main.go`
- `cmd/holon/runner.go`
- tests: `cmd/holon/runner_test.go`

**Acceptance**
- `holon run --agent <bundle.tar.gz> ...` works.
- `holon run --agent-bundle <bundle.tar.gz> ...` continues to work (compat).

### Environment variables
- [x] Add a new env var to mirror `--agent` (e.g. `HOLON_AGENT`).
- [x] Keep legacy env var(s) as aliases (e.g. `HOLON_AGENT_BUNDLE`) and document precedence.

**Acceptance**
- Precedence is documented and covered by unit tests.

### GitHub Action (`action.yml`)
- [x] Add a new input `agent` (optional) and pass it to `holon run --agent`:
  - precedence: `inputs.agent` > `HOLON_AGENT` > auto-build bundle
- [x] Keep current behavior as default:
  - if `agent` is empty, build a bundle from `agents/claude` and pick the latest `dist/agent-bundles/*.tar.gz`
  - print a one-line migration hint in logs

**Files**
- `action.yml`
- `.github/workflows/holon-issue.yml` (only if needed for examples)

**Acceptance**
- Existing workflows continue to work without changes.
- A workflow can set `with: agent: ...` to override.

## Phase 3 — Internal Renames (optional, later)
- [ ] Rename internal structs/fields to match public terms (optional cleanup):
  - `AgentBundle`/`agent-bundle` wording → `Agent` (or `AgentRef`)
  - `agentBundlePath` → `agent` (or `agentRef`) in CLI plumbing
  - remaining “adapter” wording in code/logs → “agent”
- [ ] Keep package names stable until churn is acceptable (e.g. `pkg/runtime/docker` can stay).

**Acceptance**
- Internal naming is consistent, but external compatibility remains intact.

## Phase 4 — Agent Bundle Resolver (enables npm/binary later)
- [ ] Define an agent reference format for `--agent` (start simple, keep extendable):
  - MVP: file path to a `.tar.gz` agent bundle
  - future: prefixes like `file:...`, `npm:...`, `http(s):...`
- [ ] Implement a resolver interface (keep Docker as the runner sandbox, not as the agent distribution):
  - `file` resolver (current behavior: local bundle archive)
  - `npm` resolver: design stub / behind a feature flag (install bundle at run time)
  - (optional) `http(s)` resolver: only if we also define integrity checks (hash/signature) to avoid supply-chain risk

**Acceptance**
- No behavior change for current users (local bundle + Docker runner).
- Code structure allows adding npm-distributed agent bundles without redesigning flags.

## Phase 5 — Publishers (boundary first, features later)
- [ ] Keep “publisher” as the external layer (workflows/scripts) that consumes outputs.
- [ ] Document the MVP publishers we rely on today:
  - Patch Publisher: apply `diff.patch` + commit + PR update
  - Summary Publisher: post `summary.md` to step summary / PR body
  - Review Publisher (future): publish `review.json` as PR review

**Acceptance**
- Clear boundary: agents generate outputs; publishers apply them to GitHub.

## Loose Ends (nice-to-have cleanup)
- [x] Rename build/test target wording for consistency (`make test-agent`).
- [x] Sweep `CLAUDE.md` / `AGENTS.md` for remaining “adapter” wording where it’s user-facing (migration notes can remain).
