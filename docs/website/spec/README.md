---
title: Runtime specs
summary: Current implementation-facing runtime contracts for maintainers and contributors.
order: 1
---

# Runtime specs

Spec pages describe the **current runtime contract** — what the Holon runtime
actually does today, verified against implementation and tests.

Specs are not tutorials or user guides. They are authoritative contracts for
contributors, maintainers, and integrators who need to understand or change
how the runtime behaves.

## How specs fit into the documentation model

| Layer | What | Audience |
|-------|------|----------|
| [Guides](/guides/) | Task-oriented workflows | Users |
| [Concepts](/concepts/) | Mental model | Users, evaluators |
| [Reference](/reference/) | CLI, config, control-plane snapshots | Users, integrators |
| **Specs** (here) | Current runtime contracts | Maintainers, contributors |
| [RFCs](https://github.com/holon-run/holon/tree/main/docs/rfcs) | Design records and rationale | Maintainers |

Specs bridge the gap between user-facing docs and RFC design history. When an
RFC stabilizes into runtime behavior, the current contract is extracted here.
The RFC remains the design record; the spec is the living contract.

## Reading a spec

Each spec page follows a consistent shape:

- **Contract** — the normative behavior the runtime implements.
- **Validation** — how the contract was checked against implementation, tests,
  and RFCs.
- **RFCs** — linked source design records.
- **Known gaps** — tracked follow-up issues for unresolved drift.

## Current spec pages

<!-- INDEX:START -->

- [Agent state](./agent-state.md)
  Current agent state, lifecycle labels, runtime projection, and user-facing display contract.
  <!-- mdorigin:index kind=article -->

- [Work items](./work-items.md)
  Current WorkItem lifecycle, focus, readiness, planning, blocking, and completion contract.
  <!-- mdorigin:index kind=article -->

- [Scheduler](./scheduler.md)
  Current scheduler input, runnable/waiting decisions, WorkItem readiness, and wake/sleep boundaries.
  <!-- mdorigin:index kind=article -->

- [Wake and continuation](./wake-and-continuation.md)
  Current trigger classification, external ingress capabilities, continuation resolution, and wake/sleep lifecycle.
  <!-- mdorigin:index kind=article -->

- [Tasks](./tasks.md)
  Current task lifecycle, background blocking, terminal re-entry, and command/child-agent supervision contract.
  <!-- mdorigin:index kind=article -->

- [Tools](./tools.md)
  Current model-facing tool families, authority boundaries, input/result contracts, and deprecated surfaces.
  <!-- mdorigin:index kind=article -->

- [Workspace and execution](./workspace-and-execution.md)
  Current workspace identity, agent home, execution roots, worktrees, and host-local policy contract.
  <!-- mdorigin:index kind=article -->

- [Trust and provenance](./trust-and-provenance.md)
  Current origin classification, admission/authentication, authority, and provenance tracking contract.
  <!-- mdorigin:index kind=article -->

<!-- INDEX:END -->

## Relationship to `docs/runtime-spec.md`

[`docs/runtime-spec.md`](https://github.com/holon-run/holon/blob/main/docs/runtime-spec.md)
is now an aggregate index that maps the original v0 monolithic spec to the
current focused spec pages. It no longer contains normative content.

**Focused spec pages here are the sole authoritative implementation-facing
contracts.** When a topic has a dedicated spec page, that page is the
current authority.

## For contributors

When you change runtime behavior, update the relevant spec page alongside the
RFC. Spec pages are verified against implementation — if the implementation
and spec disagree, fix the one that is wrong and open an issue for the other.
