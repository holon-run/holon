# RFC Implementation Matrix

Status: living implementation tracking note
Last reviewed: 2026-05-12

This matrix tracks how current RFC contracts map to implementation, verification,
and follow-up notes. It is intentionally non-normative: the RFC files under
`docs/rfcs/` remain the contract source of truth, and implementation decisions
under `docs/implementation-decisions/` capture durable design choices.

## RFC handles

Yes: every RFC should have one stable, unique handle so runtime work, issues,
PRs, tests, and implementation notes can refer to the same contract without
depending on title wording.

Handle convention:

- Use `rfc-` plus the RFC filename stem in lowercase kebab form, for example
  `docs/rfcs/work-item-runtime-model.md` becomes
  `rfc-work-item-runtime-model`.
- Handles are immutable after first assignment. If a file is renamed, keep the
  old handle and update this matrix.
- Do not renumber existing RFCs just to create handles. Numeric RFC ids can be
  introduced later, but the handle should remain the durable cross-reference.
- New RFCs should add a `Handle: rfc-...` metadata line near the top of the RFC
  when authored or next materially touched. Until then, this matrix is the
  handle registry.

## Status legend

- `Implemented`: the contract is represented in current code and has ordinary
  verification coverage.
- `Partial`: important pieces exist, but gaps or governance work remain.
- `Proposed`: mostly design-level; implementation is not yet established.
- `Needs Review`: status is unclear or the contract should be reconciled with
  current runtime behavior before further work.

## Matrix

| Handle | RFC | Status | Implementation anchors | Verification anchors | Notes / gaps | Related decisions |
|---|---|---:|---|---|---|---|
| `rfc-agent-and-workspace-memory` | [agent-and-workspace-memory](rfcs/agent-and-workspace-memory.md) | Partial | `src/context/`, agent/workspace memory surfaces | `cargo test` | Needs memory retention/governance reconciliation. See [memory-and-compaction](rfc-implementation-notes/memory-and-compaction.md). | 052 |
| `rfc-agent-control-plane-model` | [agent-control-plane-model](rfcs/agent-control-plane-model.md) | Partial | `src/runtime/`, agent-plane tool surfaces | `cargo test` | Core model exists; authority/admission edges still need hardening. See [control-plane-and-delegation](rfc-implementation-notes/control-plane-and-delegation.md). | 019, 034 |
| `rfc-agent-delegation-tool-plane` | [agent-delegation-tool-plane](rfcs/agent-delegation-tool-plane.md) | Partial | spawn/delegation tool surfaces, task supervision | `cargo test` | Delegation works; workflow policy is partly convention-driven. See [control-plane-and-delegation](rfc-implementation-notes/control-plane-and-delegation.md). | 019, 042, 043 |
| `rfc-agent-home-directory-layout` | [agent-home-directory-layout](rfcs/agent-home-directory-layout.md) | Partial | agent home initialization/projection | `cargo test` | Layout exists; lifecycle cleanup and migration rules should stay explicit. | 019, 043 |
| `rfc-agent-initialization-and-template` | [agent-initialization-and-template](rfcs/agent-initialization-and-template.md) | Partial | agent initialization, template loading | `cargo test` | Template inheritance and durable local edits need periodic contract review. | 019 |
| `rfc-agent-profile-model` | [agent-profile-model](rfcs/agent-profile-model.md) | Partial | profile/model/runtime identity projection | `cargo test` | Implemented in runtime surfaces; policy mapping remains related to auth boundary. | 004, 005 |
| `rfc-apply-patch-unified-diff-contract` | [apply-patch-unified-diff-contract](rfcs/apply-patch-unified-diff-contract.md) | Implemented | ApplyPatch tool parser/result envelope | `cargo test` | Keep schema/result envelope aligned with tool docs. | 014, 021, 032 |
| `rfc-command-tool-family` | [command-tool-family](rfcs/command-tool-family.md) | Implemented | `ExecCommand`, `TaskOutput`, command task lifecycle | `cargo test` | Continue auditing long-running task handoff semantics. See [tool-contracts](rfc-implementation-notes/tool-contracts.md). | 017, 018, 023 |
| `rfc-continuation-trigger` | [continuation-trigger](rfcs/continuation-trigger.md) | Partial | continuation matching, queued work reactivation | `cargo test` | Needs continued reconciliation with waiting plane and external triggers. | 013, 044 |
| `rfc-default-trust-auth-and-control` | [default-trust-auth-and-control](rfcs/default-trust-auth-and-control.md) | Partial | origin/trust classification, control-plane checks | `cargo test` | Main gap: systematic auth/admission enforcement. See [policy-and-execution-boundary](rfc-implementation-notes/policy-and-execution-boundary.md). | 004, 005 |
| `rfc-event-stream-interface` | [event-stream-interface](rfcs/event-stream-interface.md) | Partial | event stream/operator projection | `cargo test` | Needs stable remote-facing contract and replay boundaries. See [event-and-operator-transport](rfc-implementation-notes/event-and-operator-transport.md). | 012 |
| `rfc-exec-command-batch` | [exec-command-batch](rfcs/exec-command-batch.md) | Implemented | `ExecCommandBatch` tool surface | `cargo test` | Keep batch restrictions aligned with command family envelope. | 014, 021, 032 |
| `rfc-execution-policy-and-virtual-execution-boundary` | [execution-policy-and-virtual-execution-boundary](rfcs/execution-policy-and-virtual-execution-boundary.md) | Partial | workspace execution roots, local process policy | `cargo test` | Runtime reports boundary shape, but host containment is not fully enforced. See [policy-and-execution-boundary](rfc-implementation-notes/policy-and-execution-boundary.md). | 005, 016, 017, 025 |
| `rfc-extensible-model-provider-configuration` | [extensible-model-provider-configuration](rfcs/extensible-model-provider-configuration.md) | Partial | provider registry/config loading | `cargo test` | Provider attempts/failure artifacts exist; configuration contract needs continued tightening. | 032 |
| `rfc-external-trigger-capability` | [external-trigger-capability](rfcs/external-trigger-capability.md) | Partial | external trigger tools, waiting intents | `cargo test` | Needs lifecycle governance and stale trigger cleanup rules. See [work-items-and-waiting-plane](rfc-implementation-notes/work-items-and-waiting-plane.md). | 044 |
| `rfc-instruction-loading` | [instruction-loading](rfcs/instruction-loading.md) | Partial | agent/workspace AGENTS loading | `cargo test` | Loading works; precedence/provenance should remain explicit in projections. | 016 |
| `rfc-interactive-command-continuation` | [interactive-command-continuation](rfcs/interactive-command-continuation.md) | Implemented | interactive command task continuation | `cargo test` | Verify TTY/process lifecycle with command family changes. | 017, 018 |
| `rfc-long-lived-context-memory` | [long-lived-context-memory](rfcs/long-lived-context-memory.md) | Partial | context compaction/memory surfaces | `cargo test` | Needs governance around what becomes durable memory. See [memory-and-compaction](rfc-implementation-notes/memory-and-compaction.md). | 052 |
| `rfc-objective-delta-and-acceptance-boundary` | [objective-delta-and-acceptance-boundary](rfcs/objective-delta-and-acceptance-boundary.md) | Partial | work item/objective tracking | `cargo test` | Related concepts exist through work items; acceptance-boundary projection needs review. | 015, 034 |
| `rfc-openai-remote-compaction` | [openai-remote-compaction](rfcs/openai-remote-compaction.md) | Needs Review | model/provider context management | `cargo test` | Reconcile with current provider metadata and local compaction behavior. | 052 |
| `rfc-operator-wait-and-intervention` | [operator-wait-and-intervention](rfcs/operator-wait-and-intervention.md) | Partial | operator notifications, waiting posture | `cargo test` | Needs stable operator-facing intervention semantics. See [event-and-operator-transport](rfc-implementation-notes/event-and-operator-transport.md). | 012, 044 |
| `rfc-remote-operator-transport-and-delivery` | [remote-operator-transport-and-delivery](rfcs/remote-operator-transport-and-delivery.md) | Proposed | operator/event transport surfaces | `cargo test` | Main open area: remote delivery API and trust-preserving replay. See [event-and-operator-transport](rfc-implementation-notes/event-and-operator-transport.md). | 012 |
| `rfc-result-closure` | [result-closure](rfcs/result-closure.md) | Implemented | closure outcome/runtime posture | `cargo test` | Continue keeping closure separate from task status. | 012, 018 |
| `rfc-runtime-configuration-surface` | [runtime-configuration-surface](rfcs/runtime-configuration-surface.md) | Partial | config/model/provider surfaces | `cargo test` | Needs review against current model/provider configuration behavior. | 032 |
| `rfc-runtime-scheduler-contract` | [runtime-scheduler-contract](rfcs/runtime-scheduler-contract.md) | Partial | `SchedulerProjection`, `decide_next_action`, `SchedulerDecisionExecutor`, task transition reducer, work-queue idempotency keys, scheduler replay fixtures | `cargo test scheduler --quiet`; focused continuation, wake-hint, memory-refresh, and operator-interjection tests | Main gap-closing PRs are landed. Remaining follow-up is reducer cleanup around turn-loop safe-point interjection, explicit control/bootstrap posture authority, and fallback duplicate evidence. See [runtime-scheduler-contract](rfc-implementation-notes/runtime-scheduler-contract.md). | 012, 039, 044 |
| `rfc-skill-discovery-and-activation` | [skill-discovery-and-activation](rfcs/skill-discovery-and-activation.md) | Partial | skill discovery/activation surfaces | `cargo test` | Installation/management API should keep workspace vs agent scope clear. | 016 |
| `rfc-task-surface-narrowing` | [task-surface-narrowing](rfcs/task-surface-narrowing.md) | Implemented | task status/output/control tool split | `cargo test` | Keep previews/artifacts separated from lifecycle metadata. | 021, 032 |
| `rfc-tool-contract-consistency` | [tool-contract-consistency](rfcs/tool-contract-consistency.md) | Partial | tool schema/result conventions | `cargo test` | Ongoing audit needed across all tools. See [tool-contracts](rfc-implementation-notes/tool-contracts.md). | 014, 021, 032 |
| `rfc-tool-result-envelope` | [tool-result-envelope](rfcs/tool-result-envelope.md) | Implemented | shared tool result envelope | `cargo test` | Keep model-reentry receipts bounded and canonical results structured. | 021 |
| `rfc-tool-surface-layering` | [tool-surface-layering](rfcs/tool-surface-layering.md) | Implemented | tool family/layer separation | `cargo test` | Continue preventing control-plane/task/workspace shape drift. | 014, 021, 032 |
| `rfc-tui-command-surface` | [tui-command-surface](rfcs/tui-command-surface.md) | Partial | TUI slash/command projection | `cargo test` | UI remains secondary; command surface should not define runtime contracts. | 032 |
| `rfc-turn-local-context-compaction` | [turn-local-context-compaction](rfcs/turn-local-context-compaction.md) | Partial | request projection/context compaction | `cargo test` | Keep as projection behavior, not durable memory replacement. See [memory-and-compaction](rfc-implementation-notes/memory-and-compaction.md). | 052 |
| `rfc-waiting-plane-and-reactivation` | [waiting-plane-and-reactivation](rfcs/waiting-plane-and-reactivation.md) | Partial | sleep/wake/waiting intents/reactivation | `cargo test` | Needs external trigger lifecycle and work-item anchoring discipline. See [work-items-and-waiting-plane](rfc-implementation-notes/work-items-and-waiting-plane.md). | 013, 018, 044 |
| `rfc-work-item-runtime-model` | [work-item-runtime-model](rfcs/work-item-runtime-model.md) | Partial | work item tools/runtime scheduling | `cargo test` | Runtime exists; workflow discipline is still partly instruction-level. See [work-items-and-waiting-plane](rfc-implementation-notes/work-items-and-waiting-plane.md). | 015, 034, 044 |
| `rfc-workspace-binding-and-execution-roots` | [workspace-binding-and-execution-roots](rfcs/workspace-binding-and-execution-roots.md) | Implemented | workspace binding/execution root projection | `cargo test` | Continue distinguishing shell cwd from active workspace. | 016, 025 |
| `rfc-workspace-entry-and-projection` | [workspace-entry-and-projection](rfcs/workspace-entry-and-projection.md) | Implemented | workspace projection, occupancy, active workspace | `cargo test` | Keep direct/isolated roots explicit in operator projection. | 016, 025 |
