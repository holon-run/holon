# RFC Implementation Gap Analysis

This note records a point-in-time comparison between Holon's current RFC surface
and the Rust runtime implementation. It is an implementation status report, not
a normative runtime contract. If this note conflicts with an RFC, the RFC wins.

## Current Documentation And Code Shape

The repository already separates contracts, implementation choices, and runtime
code clearly:

- `docs/rfcs/` contains the canonical runtime proposals and contracts for
  agent control, provenance, work items, delegation, tools, workspace binding,
  execution policy, event streams, and provider/runtime integration.
- `docs/runtime-spec.md`, `docs/architecture-overview.md`,
  `docs/project-goals.md`, and roadmap documents provide entry points and
  project-level framing.
- `docs/implementation-decisions/` records short implementation-specific
  choices that do not belong in canonical RFCs.
- `src/types.rs` defines most shared runtime objects: agent identity, message
  envelopes, origin/trust/authority labels, tasks, closures, waiting intents,
  external triggers, work items, queue entries, and tool audit records.
- `src/runtime/` implements turn execution, closure handling, task and command
  task lifecycle, callbacks, operator surfaces, workspace runtime behavior,
  provider turns, and subagent support.
- `src/tool/` owns tool catalog, dispatch, result envelopes, schemas, and
  tool-specific implementations such as `ApplyPatch`.
- `src/system/` contains local process, file, workspace, and host policy
  support.
- `src/http.rs`, `src/client.rs`, and `src/tui.rs` expose local control,
  event-stream, and operator-facing projections.
- `src/provider/` contains provider attempt, token, transport, and failure
  handling support.

## Areas That Are Close To The RFC Direction

### Agent And Control Plane

The implementation has concrete support for agent identity and profile-driven
runtime behavior. `AgentIdentityRecord` and related types model visibility,
ownership, profile preset, parent supervision, tool-family restrictions, and
spawn behavior. `SpawnAgent` also reflects the current split between
parent-supervised private children and self-owned public named agents.

### Message Envelope And Provenance

The runtime has first-class structures for origin, trust, priority, delivery
surface, admission context, authority class, correlation identifiers, and
execution provenance. This aligns with the RFC direction that provenance should
be recorded explicitly rather than inferred from transport-specific details.

### Queue, Wake, Sleep, And Closure

Queue-centered execution is implemented through queue records, closure records,
waiting reasons, continuation decisions, and explicit terminal sleep/closure
handling. The runtime model already treats sleep, continuation, wake, and result
closure as visible state transitions rather than hidden background behavior.

### Work Item Runtime

Work items are implemented as durable runtime records with objective, plan,
plan status, todo list, blockers, active/current focus, result summaries, and
tool support for create, pick, read, update, list, and complete operations.
This substantially matches the work-item RFC direction.

### Task Lifecycle

Tasks have explicit kinds, lifecycle status, output paths, continuation hints,
and tool operations for status, output, input, and stop. Command tasks and
child-agent supervision handles are represented as managed runtime tasks rather
than ad hoc process state.

### Delegation

The runtime distinguishes public named agents from private supervised child
agents, records parent/child relationships, and supports delegated work items.
The current shape matches the core delegation RFC direction.

### External Triggers And Waiting

External trigger records, waiting intents, delivery modes, scope, and lifecycle
tools exist. The current tool contract already requires work-item scoped
triggers to be anchored to a current work item.

### Tool Layering And Result Envelopes

Tools are grouped by capability planes and use structured result envelopes,
audit records, model-visible summaries, and schema generation. This is broadly
aligned with the tool-surface layering and tool-contract consistency RFCs.

### Workspace Binding

Workspace roots, execution roots, access modes, isolation labels, and workspace
occupancy are represented explicitly instead of relying only on shell `cwd`.

### Event Stream And Operator Projection

The runtime has local HTTP/client/TUI surfaces and an event stream shape for
operator-facing projection. This provides a base for the event-stream RFCs,
even though remote/operator transport contracts remain less mature than the
local runtime surfaces.

### Provider Attempts And Failure Artifacts

Provider attempts, retry classification, token accounting, compatible-provider
support, and failure artifact normalization are backed by implementation
decisions and Rust modules.

## Main Gaps

### Missing RFC Implementation Matrix

The largest collaboration gap is not an individual runtime feature. It is the
absence of a maintained matrix that connects each RFC to current implementation
anchors, tests, implementation decisions, and open gaps. Without that map, it
is hard to tell which RFCs are implemented, partially implemented, superseded
by later decisions, or still purely proposed.

### Policy And Authorization Enforcement

Provenance and authority labels are modeled strongly, but runtime enforcement
is still lighter than the policy/authentication RFC direction implies. The next
step is to make admission, trust, authority, and tool capability checks easier
to audit as runtime-enforced invariants rather than prompt-level discipline.

### Execution Boundary Hardening

The execution policy and virtual boundary concepts are represented, but the
local backend still behaves primarily as a local execution environment with
policy surfaces around it. The hard boundary, sandbox expectations, denial
paths, and cross-backend parity need further implementation and tests.

### Work Item And Delegation Workflow Discipline

Work items and child-agent delegation exist, but parts of the intended workflow
remain process discipline instead of runtime-enforced behavior. Examples
include avoiding duplicate child agents for the same objective, reusing an
existing child branch for follow-up work, PR/review/cleanup discipline, and
projecting parent/child work state cleanly.

### External Trigger Lifecycle Governance

External trigger creation and cancellation are implemented, but lifecycle
governance needs more end-to-end hardening: stale trigger cleanup, work item
completion or focus-switch reconciliation, capability audit trails, and
integration tests for `wake_hint` versus `enqueue_message` behavior.

### Remote Operator And Event Transport Contract

The local runtime, HTTP, client, and TUI surfaces are usable, but the public
contract for remote operator transport, acknowledgements, retries, failure
semantics, and provenance preservation is still less explicit than the local
runtime model.

### Tool Contract Consistency

The runtime has a structured tool layer, but consistency remains a risk across
canonical JSON results, model-visible receipts, provider schemas, artifact
references, and historical tool-call shapes. This is especially important for
tools whose surface evolved over time, such as `ApplyPatch`, command tasks,
`TaskStatus`, and `TaskOutput`.

### Memory And Compaction Governance

Long-lived context, memory, and compaction are less mature than the central
runtime loop. Remaining gaps include clearer agent-home versus project-memory
boundaries, automatic memory extraction and review, cache/index ownership,
search provenance and ranking, and recovery after compaction.

## Suggested Priorities

1. Build and maintain an RFC implementation matrix.
2. Strengthen policy, admission, authorization, and execution-boundary
   enforcement.
3. Move more work-item and delegation workflow discipline into explicit runtime
   state transitions.
4. Reconcile external trigger lifecycle behavior with work item completion,
   focus changes, and cancellation.
5. Stabilize the remote operator and event transport contract.
6. Audit tool contract consistency across schemas, receipts, canonical results,
   and artifact references.
7. Mature memory, compaction, and search governance.

## Where The Implementation Matrix Should Live

The matrix should be a separate status artifact, not embedded into each RFC.
`docs/rfcs/README.md` explicitly frames RFCs as architectural proposals and
contract documents, not implementation status reports. Therefore the best
location is a top-level document such as:

```text
docs/rfc-implementation-matrix.md
```

That document can cross-reference RFCs, implementation-decision notes, code
modules, tests, and open issues without turning the RFC directory into a status
tracker.

Recommended columns:

- RFC
- status: proposed, partial, implemented, superseded, or retired
- implementation anchors: modules, types, tools, or commands
- verification anchors: tests, scripts, or manual checks
- related implementation decisions
- remaining gaps or open issues
- last reviewed

RFCs should be updated only when the contract itself changes, when a contract
is stale or superseded, or when a gap reveals that the documented architecture
no longer matches the intended runtime model. If an implementation choice
matters for future maintenance but does not change the public contract, it
belongs in `docs/implementation-decisions/` instead.

Once the matrix exists, `docs/rfcs/README.md` can link to it as a
non-normative status index, while keeping each RFC focused on the contract.

## Overall Assessment

The Rust runtime is not far behind the RFC direction. The core model is
substantially present: agent identity, provenance, queueing, closure, work
items, tasks, delegation, tool planes, workspace binding, and local event
surfaces all have implementation anchors.

The remaining work is mainly to move from explicit modeling to enforceable
invariants, from local runtime surfaces to stable multi-entry transport
contracts, and from scattered RFC/implementation-decision evolution to a
maintained implementation-status matrix.
