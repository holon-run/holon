# Holon Runtime Spec — Aggregate Index

> **Status:** This document is the aggregate index for the former v0 runtime
> spec. Individual topic contracts have been extracted into focused spec pages
> under [`docs/website/spec/`](./website/spec/). **Focused spec pages are the
> authoritative current contracts.** RFCs remain the design-record source of
> truth. This page maps original sections to their current homes.
>
> **Reading path:** For user-facing documentation, start at the
> [documentation website](https://holon.run). For design rationale and
> historical decisions, see [`docs/rfcs/`](./rfcs/). For current
> implementation-facing contracts, see [`docs/website/spec/`](./website/spec/).

## Focused spec pages (authoritative)

| Spec page | Covers |
|-----------|--------|
| [Agent state](./website/spec/agent-state.md) | Agent lifecycle, status, scheduling posture, user-facing projection |
| [Work items](./website/spec/work-items.md) | WorkItem lifecycle, focus, readiness, planning, blocking, completion |
| [Scheduler](./website/spec/scheduler.md) | Scheduler inputs, runnable/waiting decisions, WorkItem readiness, wake/sleep |
| [Wake and continuation](./website/spec/wake-and-continuation.md) | Trigger classification, wake hints, external triggers, continuation resolution |
| [Tasks](./website/spec/tasks.md) | Task lifecycle, terminal re-entry, command/child-agent supervision |
| [Tools](./website/spec/tools.md) | Tool families, authority boundaries, input/result contracts, deprecated surfaces |
| [Workspace and execution](./website/spec/workspace-and-execution.md) | Workspace identity, agent home, worktrees, host-local policy |
| [Trust and provenance](./website/spec/trust-and-provenance.md) | Origin classification, admission, authority, provenance tracking |

## Section migration map

### Agent model

| Original section | Current home |
|------------------|--------------|
| Agent State | [Agent state spec](./website/spec/agent-state.md) |
| Agent Initialization Contract | Implementation detail; see `src/runtime/agent_init.rs` and `builtin_templates/` |
| Agent Identity And Visibility Contract | [Agent state spec](./website/spec/agent-state.md) — lifecycle, identity, visibility, ownership, profile presets |
| Agent Model Selection | [Agent state spec](./website/spec/agent-state.md) |
| Agent Inspection Surface Contract | Control-plane API; see `/agents/list`, `/agents/:id/status`, `/agents/:id/state` |
| Local Operator Troubleshooting Contract | Operational guide; CLI tools (`holon daemon status`, `holon run --json`, etc.) |
| AGENTS.md Loading Contract | [Agent state spec](./website/spec/agent-state.md) — prompt assembly; also `holon debug prompt` |
| Skill Discovery And Activation Contract | [Agent state spec](./website/spec/agent-state.md) — catalog discovery, scope, activation |

### Message and trust model

| Original section | Current home |
|------------------|--------------|
| Message Envelope | [Trust and provenance spec](./website/spec/trust-and-provenance.md) and RFCs |
| Message Kinds | [Trust and provenance spec](./website/spec/trust-and-provenance.md) and `src/types.rs` |
| Origins | [Trust and provenance spec](./website/spec/trust-and-provenance.md) |
| Authority Classes And Compatibility Trust | [Trust and provenance spec](./website/spec/trust-and-provenance.md) |
| Priority | [Trust and provenance spec](./website/spec/trust-and-provenance.md) |
| Message Body | [Trust and provenance spec](./website/spec/trust-and-provenance.md) |

### Turn and continuation

| Original section | Current home |
|------------------|--------------|
| Core Runtime Model | [Scheduler spec](./website/spec/scheduler.md), [Wake and continuation spec](./website/spec/wake-and-continuation.md) |
| Turn Terminal | [Wake and continuation spec](./website/spec/wake-and-continuation.md) |
| Closure Decision | [Wake and continuation spec](./website/spec/wake-and-continuation.md) |
| Continuation Resolution | [Wake and continuation spec](./website/spec/wake-and-continuation.md) |
| Queue Semantics | [Scheduler spec](./website/spec/scheduler.md) |
| Wake / Sleep Lifecycle | [Wake and continuation spec](./website/spec/wake-and-continuation.md) |
| SystemTick: Runtime-Owned Scheduling | [Scheduler spec](./website/spec/scheduler.md) |
| Wake Hints: Pure Wake Signals | [Wake and continuation spec](./website/spec/wake-and-continuation.md) |
| External Trigger Capabilities | [Wake and continuation spec](./website/spec/wake-and-continuation.md); also [RFC: external-trigger-capability](./rfcs/external-trigger-capability.md) |

### Tools and provider contracts

| Original section | Current home |
|------------------|--------------|
| Provider Tool Schema Contract | [Tools spec](./website/spec/tools.md), `src/tool/spec.rs` |
| Tool Error Envelope Contract | [Tools spec](./website/spec/tools.md); also [RFC: tool-result-envelope](./rfcs/tool-result-envelope.md) |
| Repo Inspection Contract | [Workspace and execution spec](./website/spec/workspace-and-execution.md); shell-first inspection is also part of runtime prompt policy |
| Provider Transport Contract | Implementation detail; provider selection/retry/token-usage in `src/provider/` |
| Failure Artifact Envelope | Implementation detail; `FailureArtifact` shape in `src/types.rs` |
| Provider Prompt Frame | Implementation detail; prompt assembly in `src/prompt/` |

### Work items and memory

| Original section | Current home |
|------------------|--------------|
| Work-Item Persistence Foundation | [Work items spec](./website/spec/work-items.md) |
| Work-Item Tool Contract | [Work items spec](./website/spec/work-items.md) |
| Memory Search Index | Implementation detail; `MemorySearch`/`MemoryGet` contract in tool schema inventory |

### Execution and tasks

| Original section | Current home |
|------------------|--------------|
| Execution Binding | [Workspace and execution spec](./website/spec/workspace-and-execution.md) |
| Worktree Session | [Workspace and execution spec](./website/spec/workspace-and-execution.md) |
| Background Task Model | [Tasks spec](./website/spec/tasks.md) |
| Background Task Recovery | [Tasks spec](./website/spec/tasks.md) |
| Brief Output Contract | [Work items spec](./website/spec/work-items.md) — completion reports; also `src/types.rs` `BriefRecord` |

### Operator and operational surfaces

| Original section | Current home |
|------------------|--------------|
| Local Operator Console Contract | Implementation detail; `holon tui` contract in `src/tui/` |
| Local Daemon Lifecycle Contract | Operational surface; `holon daemon` CLI reference |

### Historical / deferred

| Original section | Current home |
|------------------|--------------|
| Terminology Note | Replaced by [RFC: external-trigger-capability](./rfcs/external-trigger-capability.md) |
| Scope | This index page replaces the original scope declaration |
| Logging And Audit | General guidance; no standalone spec yet |
| Open Decisions For Next Revision | Historical; superseded by GitHub issues and RFCs |

## Content not yet extracted into focused specs

The following areas are implementation-level contracts with no dedicated
focused spec page. They remain in source code, RFCs, or CLI reference:

- **Provider transport contract** (selection, retry, fallback, token usage):
  `src/provider/`, `src/types.rs` `ProviderAttemptTimeline`
- **Provider prompt frame** (prompt assembly and lowering):
  `src/prompt/`, `src/provider/openai.rs`, `src/provider/anthropic.rs`
- **Failure artifact envelope** (operator-facing failure normalization):
  `src/types.rs` `FailureArtifact`
- **Agent initialization** (template selection, agent_home materialization):
  `src/runtime/agent_init.rs`, `builtin_templates/`
- **Memory search index** (FTS indexing, CJK, rebuild markers):
  `src/memory/`, tool schema inventory
- **Local operator console** (TUI contract):
  `src/tui/`
- **Local daemon lifecycle** (`holon daemon` commands):
  CLI reference, `src/daemon/`

## Historical note

The original `docs/runtime-spec.md` (committed 2025-01-25, ~2860 lines) was
the first runtime contract for Holon. It defined the core lifecycle, message
model, and tool surface in a single aggregate document. Over time, individual
topics were extracted into focused spec pages under `docs/website/spec/`,
each verified against implementation and tests.

The focused spec pages are now the authoritative implementation-facing
contracts. This aggregate index is the navigation map from the original
monolithic spec to the current focused contracts. It does not contain
normative content.

For the git history of the original v0 spec, see:
```bash
git log -- docs/runtime-spec.md
```
