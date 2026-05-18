# Documentation Cleanup Audit

Date: 2026-05-15

**Last reconciled**: 2026-05-15 (v0.13.0, commit range up to origin/main)

## Completion Status

**Reconciled with documentation layers (#1012)**: 2026-05-09 ✅

The three-layer documentation model from #1006/#1012 is now documented at
`docs/website/concepts/documentation-layers.md`. This audit remains the
canonical maintainer-facing tracking document for vocabulary drift, archive
candidates, and RFC merge targets.

**Executed**: 2026-05-07

The high-confidence archival and roadmap consolidation recommendations from this
audit have been executed. Superseded documents have been moved to
`docs/archive/` with appropriate prefixes (superseded-, historical-, reference-,
etc.).

See `docs/archive/README.md` for details on the archive structure.

**Docs issues #1153–#1168**: In progress (2026-05-15)

Documentation issues from the #1006 umbrella are being resolved in individual
PRs. Completed so far:

| Issue | Document | Status |
|-------|----------|--------|
| #1154 | `docs/website/guides/solve.md` | ✅ PR #1172 |
| #1155 | `docs/website/reference/cli.md` (debug scheduler-fixture) | ✅ PR #1181 |
| #1156 | `docs/website/guides/tui.md` (new) | ✅ PR #1182 |
| #1157 | `docs/website/guides/agent-templates.md` (new) | ✅ PR #1183 |
| #1158 | `docs/website/concepts/memory.md` (new) | ✅ PR #1184 |
| #1159 | `docs/website/concepts/runtime-model.md` (external triggers) | ✅ PR #1185 |
| #1160 | `docs/website/concepts/runtime-model.md` (completion reports) | ✅ PR #1186 |
| #1161 | `docs/website/guides/remote-access.md` (new) | ✅ PR #1187 |
| #1162 | `docs/website/guides/workspaces.md` (new) | ✅ PR #1188 |
| #1163 | `docs/website/reference/cli.md` (deprecated control) | ✅ PR #1189 |
| #1164 | `docs/website/guides/quick-examples.md` (TUI examples) | ✅ PR #1190 |
| #1165 | `docs/website/reference/cli.md` (run --workspace-root verified) | ✅ PR #1191 |
| #1166 | `docs/website/reference/cli.md` (solve --workspace) | ✅ PR #1192 |
| #1167 | `docs/website/reference/configuration.md` (config unset) | ✅ PR #1193 |

New documentation pages created in this batch: `tui.md`, `agent-templates.md`,
`memory.md`, `remote-access.md`, `workspaces.md`.

## Summary

`docs/rfcs/` is now the canonical home for Holon's architecture contracts.

Several older top-level notes still duplicate or predate the RFC set. They
should either be archived as historical notes, reduced to short pointers, or
merged into the relevant RFC.

This audit intentionally does not move files yet. It records the recommended
cleanup plan so future doc changes can be done in small, reviewable commits.

## Recommended Cleanup Policy

- Keep `docs/rfcs/` as canonical design contracts.
- Keep `docs/implementation-decisions/` as historical ADR-style records.
- Keep runbooks and quickstarts when they describe operational workflows rather
  than architecture.
- Archive old planning notes once their remaining design content is covered by
  RFCs.
- Avoid keeping two active documents that define the same concept with
  different names.

## High-Confidence Archive Or Reduce

These documents are largely superseded by RFCs and should be archived or
reduced to a short pointer.

| Document | Recommendation | Canonical Destination |
| --- | --- | --- |
| `docs/agent-types-and-default-agent.md` | Archive after checking for any missing wording around default/named/child agents. | `docs/rfcs/agent-profile-model.md`, `docs/rfcs/agent-control-plane-model.md`, `docs/rfcs/agent-delegation-tool-plane.md` |
| `docs/callback-capability-and-providerless-ingress.md` | **Updated**: External trigger implementation landed (v0.13.0). Can now archive. | `docs/rfcs/external-trigger-capability.md` |
| `docs/agentinbox-callback-integration.md` | **Updated**: Wake hint / external trigger system now in place. Archive or keep as operational note. | `docs/rfcs/external-trigger-capability.md` |
| `docs/command-execution-and-task-model.md` | Archive after confirming command-task details are covered. | `docs/rfcs/command-tool-family.md`, `docs/rfcs/interactive-command-continuation.md`, `docs/rfcs/task-surface-narrowing.md` |
| `docs/single-agent-context-compression.md` | **Updated**: Long-lived context memory RFC now partially implemented (working memory, episodes, MemorySearch). Keep until full compaction lands. | `docs/rfcs/long-lived-context-memory.md` |
| `docs/worktree-design-roadmap.md` | Archive after extracting any remaining worktree workflow decisions. | `docs/rfcs/workspace-binding-and-execution-roots.md`, `docs/rfcs/workspace-entry-and-projection.md`, `docs/implementation-decisions/042-child-agent-task-workspace-mode.md`, `docs/implementation-decisions/043-task-owned-worktree-cleanup.md` |

## Needs Consolidation Before Archive

These documents still contain useful direction, but they overlap heavily with
newer RFCs or current implementation decisions.

| Document | What To Do |
| --- | --- |
| `docs/roadmap.md` | Archive or replace with a current roadmap. It still describes older milestones such as default trust policy by origin. |
| `docs/coding-roadmap.md` | Reduce to historical implementation roadmap. It contains stale tool names and trust-aware tool exposure wording. |
| `docs/post-benchmark-roadmap.md` | Merge still-relevant benchmark-driven priorities into `docs/next-phase-direction.md`, then archive. |
| `docs/prompt-architecture-roadmap.md` | Convert remaining prompt-system direction into a dedicated RFC if still active. |
| `docs/prompt-benchmark-decisions.md` | Keep only if it is still used as benchmark guidance; otherwise archive with benchmark notes. |
| `docs/next-phase-direction.md` | Keep as the current planning note, but update provenance/trust wording and link to relevant RFCs. |

## Keep As Active Supporting Docs

These should remain outside RFCs because they are operational guides,
implementation specs, comparison references, or review artifacts.

| Document | Reason |
| --- | --- |
| `docs/runtime-spec.md` | Active implementation-facing spec. It should be updated to align with `authority_class`, `NotifyOperator`, and External Trigger wording. |
| `docs/local-operator-troubleshooting.md` | Operational guide, not architecture. |
| `docs/agentinbox-dogfood-runbook.md` | Operational runbook for real integration dogfooding. |
| `docs/agentinbox-wake-hint-quickstart.md` | Operational quickstart. Keep, but ensure External Trigger naming remains current. |
| `docs/benchmark-guardrails.md` | Benchmark policy. Keep. |
| `docs/benchmark-plan.md` | Keep if still driving benchmark work; otherwise archive after current benchmark cycle. |
| `docs/benchmark-results.md` | Historical results. Keep or move to a benchmark archive later. |
| `docs/project-goals.md` | Product/context note. Keep unless it starts duplicating RFCs. |
| `docs/architecture-overview.md` | Short architecture overview and RFC entry point. Keep, but do not add normative details here. |

## Keep As Reference Or Research

These are useful as comparative or historical reference, but should not be read
as current Holon contract.

| Document | Recommendation |
| --- | --- |
| `docs/claude-code-reference.md` | Keep as reference; add a note that Holon contracts live in RFCs. |
| `docs/claude-vs-codex-for-holon.md` | Keep as reference/research. |
| `docs/basic-tool-comparison.md` | Keep as reference or archive if no longer used. |
| `docs/tool-surface-comparison.md` | Keep as reference or merge into tool RFC background. |
| `docs/svs402-decision.md` | Keep if tied to external benchmark/task history; otherwise archive. |

## RFC Merge Targets

The following merges should happen before moving files to `docs/archive/`.

### Agent Model

Merge any remaining content from `docs/agent-types-and-default-agent.md` into:

- `docs/rfcs/agent-profile-model.md`
- `docs/rfcs/agent-control-plane-model.md`
- `docs/rfcs/agent-delegation-tool-plane.md`

Focus on:

- default agent wording
- named agent vs child agent wording
- visibility and lifecycle semantics

### External Trigger

Merge any remaining implementation caveats from:

- `docs/callback-capability-and-providerless-ingress.md`
- `docs/agentinbox-callback-integration.md`

into:

- `docs/rfcs/external-trigger-capability.md`

Most naming has already been migrated. Remaining work is to remove or archive
the old callback/reactivation note once implementation #393 lands.

### Command And Task Surface

Merge remaining details from `docs/command-execution-and-task-model.md` into:

- `docs/rfcs/command-tool-family.md`
- `docs/rfcs/interactive-command-continuation.md`
- `docs/rfcs/task-surface-narrowing.md`

Focus on:

- command task lifecycle
- non-interactive long-running command promotion
- boundary between command execution and task orchestration

### Context Memory

Merge remaining content from `docs/single-agent-context-compression.md` into:

- `docs/rfcs/long-lived-context-memory.md`

Focus on:

- budget-aware prompt planning
- episode memory shape
- prompt cache stability

### Worktree Workflow

Either merge `docs/worktree-design-roadmap.md` into existing workspace and
delegation RFCs, or create a focused `Worktree Workflow` RFC if the remaining
workflow-level contract is still not covered.

Potential targets:

- `docs/rfcs/workspace-binding-and-execution-roots.md`
- `docs/rfcs/workspace-entry-and-projection.md`
- `docs/rfcs/agent-delegation-tool-plane.md`

## Stale Vocabulary To Fix

The following old vocabulary still appears in active or semi-active docs and
should be normalized as those docs are touched:

- `trust` / `trusted_*` / `untrusted_external` as primary concepts
- `root agent` for default agent
- `callback capability` as public concept instead of External Trigger
- `CreateReactivationChannel` / `CancelReactivationChannel` as preferred names
- `RequestOperatorInput` as phase-1 primitive
- `channel_event` as the default framing for ordinary IM/channel content
- `callback capability` → External Trigger / wake hint (✅ migrated in v0.13.0)

### Vocabulary Status Update (2026-05-15)

| Old Term | New Term | Status |
|----------|----------|--------|
| `CreateReactivationChannel` | `CreateExternalTrigger` | ✅ Renamed |
| `CancelReactivationChannel` | `CancelExternalTrigger` | ✅ Renamed |
| `callback capability` | External Trigger | ✅ Migrated |
| `trusted_*` / `untrusted_external` | Still active | Keep; runtime contract |

Do not mechanically rewrite archived historical docs unless they are being
unarchived or cited as current behavior.

## Suggested Cleanup Order

1. Add superseded headers to the high-confidence archive/reduce documents.
2. Merge any missing content into the listed RFCs.
3. Move fully superseded top-level notes into `docs/archive/`.
4. Update `docs/runtime-spec.md` to align with current RFC vocabulary.
5. Reduce roadmap sprawl by keeping `docs/next-phase-direction.md` as the
   active planning note and archiving older roadmaps.
6. Re-run link checks or at least `rg` for moved filenames before committing.

**Progress (2026-05-15):** External trigger naming is complete. Context memory implementation is partial (working memory + episodes landed; full compaction pending). Documentation for TUI, memory, external triggers, workspaces, remote access, and agent templates now exists in `docs/website/`.

## Do Not Archive Yet

Do not archive these until replacement content is verified:

- `docs/runtime-spec.md`
- `docs/next-phase-direction.md`
- `docs/local-operator-troubleshooting.md`
- `docs/agentinbox-dogfood-runbook.md`
- `docs/agentinbox-wake-hint-quickstart.md`
- `docs/implementation-decisions/`
