---
title: RFC: Deterministic Scheduler Decision Hooks
date: 2026-09-19
status: accepted
handle: rfc-deterministic-scheduler-decision-hooks
---

# RFC: Deterministic Scheduler Decision Hooks

## Summary

Holon's scheduler needs explicit internal decision boundaries before any
semantic decision provider can be integrated safely.

This RFC defines six synchronous, typed internal hook boundaries for Phase 1:

1. ingress routing;
2. wake eligibility;
3. dispatch placement;
4. interjection placement;
5. continuation disposition; and
6. semantic candidate selection.

Every hook receives canonical facts, a runtime-produced set of legal
candidates or dispositions, and the current static result. A hook may select
only from that bounded input or abstain. The deterministic runtime validates
the proposal and remains solely responsible for identity, admission, binding,
wait consumption, generation fences, atomic state transitions, settlement,
delivery, and durable commit.

Phase 1 uses only static in-process implementations and must preserve current
behavior. Phase 2 may adapt selected hooks to a provider-agnostic decision API,
but the existence of a Phase 1 hook does not grant a future provider authority
to use it.

This RFC is the contract portion of issue #3098 step A. Characterization tests
described here should be added only after this contract is accepted.

## Status And Relationship To Existing RFCs

This RFC refines decision boundaries within the canonical scheduler described
by:

- [Runtime Scheduler Contract](./runtime-scheduler-contract.md);
- [Scheduler–WorkItem Unified Execution Protocol](./scheduler-work-item-unified-execution-protocol.md);
- [Agent Activation, Settlement, and Dispatch](./agent-activation-settlement-and-dispatch.md);
- [Runtime Transition Commit Contract](./runtime-transition-commit-contract.md);
- [Wake Authority](./wake-authority.md);
- [Waiting Plane And Reactivation](./waiting-plane-and-reactivation.md);
- [Operator Interjection Safe Points](./operator-interjection-safe-points.md);
- [Continuation Trigger](./continuation-trigger.md); and
- [Scheduler Cutover Simplification](./scheduler-cutover-simplification.md).

Those RFCs remain authoritative for durable state, activation ownership, wait
identity, safe points, transition commit, settlement, delivery, and recovery.
This RFC does not replace their protocols. It defines the bounded proposal
seams inside them.

## Current Structure

The current runtime already separates several parts of the target design, but
the decision boundaries are not represented as one explicit hook catalog:

- `src/ingress.rs` normalizes an `InboundRequest` into a
  `MessageEnvelope`. The current public request shape carries an exact
  `agent_id`; callback ingress resolves an external trigger before queueing the
  message.
- `src/runtime/scheduler.rs` builds `SchedulerProjection`, represents
  canonical activation scenarios and candidates, and derives static scheduler
  decisions through functions such as `decide_next_action`.
- `src/runtime/scheduler_executor.rs` selects queue input, builds dispatch
  plans, applies admission and execution binding, invokes the model turn, and
  coordinates settlement.
- `src/runtime/waiting.rs` and `src/runtime/wake_matching.rs` own wait
  registration and wake-source matching.
- `src/runtime/message_dispatch.rs`,
  `src/runtime/operator_dispatch.rs`, and the turn modules own message
  delivery and the provider/tool safe points at which persisted interjections
  can be observed.
- the runtime database transition paths own revision checks, generation
  fences, atomic writes, durable audit evidence, and restart recovery.

The current behavior is therefore not one monolithic scheduler function.
Static choices and hard state-transition authority are distributed across
these modules. Phase 1 must expose decision seams without moving hard
authority into the seams or introducing a second scheduler.

## Problem

The runtime must eventually permit semantic components to help with ambiguous
choices, such as ranking several legal WorkItem candidates. It cannot safely
do that while:

- semantic choice is mixed with state mutation;
- exact identity paths and ambiguous selection paths share an implicit branch;
- candidate legality is recomputed differently by each caller;
- hook failure semantics are unspecified;
- stale proposals can be confused with current runtime facts; or
- a future provider can accidentally acquire authority merely because an
  interface accepts its output.

A single broad `DecisionRequest -> DecisionResponse` API in Phase 1 would make
these boundaries less clear. Different decision points have different facts,
legal outputs, bypass rules, and state-transition consequences. Phase 1 should
first make those internal contracts explicit.

## Goals

- Define all six Phase 1 decision hooks.
- Give each hook a typed context and typed proposal.
- Record each hook's invocation point, bypass conditions, static result,
  validation rules, and fallback behavior.
- Keep exact identity and hard runtime constraints deterministic.
- Make the existing static behavior the only Phase 1 implementation.
- Make hook behavior directly testable without database mutation or network
  access.
- Define the characterization matrix required before later extraction work.
- Leave a narrow adapter seam for Phase 2 without defining the Phase 2 wire
  protocol.

## Non-goals

- No model, classifier, embedding system, Jev integration, or remote provider.
- No provider-agnostic `DecisionRequest -> DecisionResponse` wire contract.
- No hook registry, plugin system, dynamic loading, runtime provider
  configuration, or per-hook authority policy.
- No timeout, budget, retry, confidence, calibration, free-text reasoning, or
  shadow-result storage.
- No change to queue ordering, wake matching, admission, activation ownership,
  settlement, delivery, or durable commit.
- No new public targetless ingress behavior merely because an ingress hook
  exists.
- No direct mutation of an executing provider context to simulate
  interjection.

## Terminology

### Canonical facts

Durable or runtime-authoritative facts read for one decision attempt, including
the relevant identity, trust, permission, lifecycle, revision, generation,
queue, wait, task, WorkItem, activation, and settlement facts.

### Candidate

A runtime-produced typed reference to a target or disposition that has passed
the deterministic preconditions required before the hook. A candidate is not a
state mutation and does not reserve its target.

### Static baseline

The result produced by the existing deterministic ordering and selection
rules. Phase 1 hooks return this result. It is also the fallback when a hook
abstains or fails validation.

### Proposal

A hook result that references only candidates or dispositions present in its
context. A proposal is advisory until the runtime validates it and successfully
commits the corresponding transition.

### Bypass

A path on which the runtime does not ask a semantic hook to choose because an
authoritative exact identity or explicit intent already determines the next
step. A bypass is still represented in tests and diagnostics.

### Runtime gate

The deterministic code and atomic transition layer that verify current facts
and own all state mutation.

## Shared Hook Contract

Phase 1 should use separate Rust context and proposal types for each hook. The
following shape is conceptual, not a shared external schema:

```rust
struct HookContext<Candidate, Baseline> {
    snapshot_identity: SnapshotIdentity,
    candidates: Vec<Candidate>,
    baseline: Baseline,
}

enum HookResult<Proposal> {
    Propose(Proposal),
    Abstain,
}
```

The common rules are:

1. Context construction reads canonical facts once for one decision attempt.
2. Deterministic code removes candidates that fail trust, permission,
   lifecycle, exact-identity, revision, generation, or other hard constraints.
3. Deterministic static logic computes the baseline before the hook runs.
4. The hook may reference only a context candidate or allowed disposition.
5. The runtime validates membership and snapshot identity before accepting the
   proposal.
6. The transition layer revalidates current facts before commit.
7. The hook does not receive a storage handle, queue writer, transition
   transaction, provider client, or arbitrary state patch.

Contexts should contain the minimum facts needed for their decision point.
They should not expose unrelated transcript text, secrets, credentials, raw
provider payloads, or mutable runtime handles.

Phase 1 hooks are:

- synchronous;
- in-process;
- side-effect free;
- deterministic for the same context;
- non-blocking except for ordinary CPU work; and
- implemented only by the static baseline.

The implementation may share internal validation helpers, candidate
references, snapshot identities, and structured reason codes. It should not
force all six hooks into one large enum or one generic request type.

## Decision Pipeline

Each non-bypassed decision follows this sequence:

1. **Capture facts.** Read the canonical facts and the revision or generation
   identities needed to prove what was observed.
2. **Apply hard filters.** Remove illegal candidates and compute the legal
   disposition set.
3. **Compute the baseline.** Run the current static rules against the captured
   facts.
4. **Invoke the hook.** The Phase 1 implementation returns the static proposal
   or `Abstain`.
5. **Validate the proposal.** Check candidate membership, allowed disposition,
   snapshot identity, and hook-specific invariants.
6. **Choose the effective result.** Use a valid proposal; otherwise use the
   static baseline from the same snapshot.
7. **Enter the runtime gate.** Revalidate current facts and attempt the atomic
   transition.
8. **Restart on stale facts.** If the gate observes a revision, generation, or
   lifecycle change, discard the whole decision attempt and rebuild it from
   fresh facts. A stale proposal and a stale baseline are never committed.

This sequence prevents fallback from becoming post-hoc normalization. The
static baseline is a first-class result computed from the same input, not a
repair applied after an invalid mutation.

## Failure, Abstention, And Fallback

The following outcomes use the static baseline from the same snapshot:

- hook unavailable;
- explicit `Abstain`;
- internal hook error;
- unknown proposal variant;
- candidate not present in the context;
- disposition not present in the allowed set; or
- a hook-specific validation failure.

The following outcomes do not reuse either the proposal or its baseline:

- revision mismatch;
- generation mismatch;
- changed wait identity;
- changed task or activation ownership;
- changed trust, permission, or lifecycle fact; or
- any other transaction precondition failure.

Those outcomes discard the attempt and follow the existing retry, defer, or
fail-closed behavior after reading fresh facts.

Errors while reading canonical facts or computing the static baseline are
runtime errors, not hook failures. They retain the existing error semantics and
must not be hidden by `Abstain`.

## Deterministic Runtime Gate

The following authority never belongs to a hook:

| Authority | Runtime responsibility |
| --- | --- |
| trusted exact target | verify and preserve explicit agent, WorkItem, task, wait, activation, and caller identities |
| trust and permission | classify ingress authority and reject unauthorized candidates |
| lifecycle | reject missing, terminal, blocked, stale, or otherwise ineligible state |
| claim and admission | claim queue input and decide whether execution may begin |
| activation ownership | bind the single agent lane and the execution attempt |
| wait trigger and consume | match the exact wait generation and atomically consume it |
| revision and generation fences | prevent stale decisions and duplicate execution |
| queue and transition mutation | write queue, task, WorkItem, wait, activation, and settlement facts |
| atomic commit | commit the complete business transition or no transition |
| settlement and delivery | classify terminal evidence, publish user-facing output, and finalize the attempt |
| replay and recovery | reconstruct authoritative state and repair incomplete transitions |

Hooks may observe bounded projections of these facts. They cannot weaken,
replace, or bypass the gate.

## Hook Catalog

### `IngressRouteDispositionHook`

#### Purpose

Choose among legal runtime targets when ingress does not carry a trusted exact
target.

#### Input

- normalized ingress origin, trust, authority, kind, priority, and delivery
  surface;
- correlation or causation identity when relevant;
- legal route candidates produced by deterministic lookup;
- exact-target and affinity evidence;
- candidate revisions needed for later validation; and
- the static route baseline.

A route candidate should be a typed reference such as an agent lifecycle,
WorkItem-owned execution target, or another already supported runtime target.
The hook cannot construct a new agent id, WorkItem id, or admission binding.

#### Output

- one candidate reference;
- an explicitly allowed unresolved or deferred disposition; or
- `Abstain`.

#### Invocation point

After ingress normalization and trust classification, after deterministic
candidate construction, and before queue ownership or execution admission is
committed.

#### Bypass conditions

- trusted exact agent target;
- trusted exact WorkItem target whose ownership is already resolved;
- exact task rejoin;
- exact wait resume; or
- any ingress surface whose contract requires an exact target and rejects the
  request before candidate construction.

#### Static baseline

Preserve the current exact-target and affinity rules. Ambiguous or targetless
input that is not routable today remains unresolved or rejected in Phase 1.
Connecting this hook does not expand the public ingress contract.

#### Rejection and fallback

Reject proposals that reference a non-candidate, cross a trust or permission
boundary, conflict with exact identity, or use stale target revisions. Semantic
failure uses the static baseline; stale facts restart the decision attempt.

### `WakeEligibilityHook`

#### Purpose

Advise whether an already matched event should wake now or remain a durable
pending wake for later reevaluation.

#### Input

- matched wake source and durable event identity;
- exact wait, task, timer, external trigger, or local continuation identity;
- target scheduling posture and lifecycle facts;
- legal wake dispositions;
- relevant wait and WorkItem revisions or generations; and
- the static wake baseline.

#### Output

- `Wake(candidate)`;
- `Defer(candidate, allowed_reason_code, recheck_condition)`; or
- `Abstain`.

The output does not consume a wait, resolve a task, enqueue a synthetic event,
or claim an activation.

There is no terminal `NoWake` outcome for a matched durable wake obligation.
`Defer` is legal only when deterministic runtime code can preserve the event as
pending and name a concrete reevaluation condition, such as a timer fire, wait
or lifecycle revision, lane release, queue-admission opportunity, or bounded
fallback recheck. Neither `Defer` nor `Abstain` acknowledges the event or
consumes its wait. `Abstain` runs the static baseline immediately.

#### Invocation point

After deterministic wake-source matching and hard eligibility filtering, but
before wait consumption, queue mutation, or activation admission.

#### Bypass conditions

- exact task result whose owner and captured WorkItem are authoritative;
- exact wait resume whose wake source and generation match;
- lifecycle or permission rejection;
- ambiguous canonical waits that must fail closed; or
- a transition already determined by explicit runtime control input.

#### Static baseline

Preserve the existing wake matching, reactivation, and fail-closed ambiguity
rules.

#### Rejection and fallback

Reject a proposal that names a different wait, target, owner, or generation;
wakes an ineligible lifecycle; or requests a disposition not present in the
allowed set. Also reject `Defer` when its reason is not allowed, when its
reevaluation condition is not durably represented, or when it would clear the
pending wake obligation. Rejection uses the static baseline.

### `DispatchPlacementHook`

#### Purpose

Choose where an already legal durable input should wait for execution.

#### Input

- selected durable input identity;
- agent lane and activation posture;
- queue and WorkItem scheduling facts;
- interjection, task-rejoin, and safe-boundary facts;
- the runtime-produced set of legal placements; and
- the static placement baseline.

#### Output

One allowed placement, for example:

- activate when admission succeeds;
- remain in the runnable queue;
- wait for the next safe boundary;
- remain held by an existing explicit wait or barrier; or
- `Abstain`.

The exact enum should contain only placements the current runtime can represent
without inventing new durable states.

#### Invocation point

After target and wake eligibility are known and before queue claim, admission,
or activation binding.

#### Bypass conditions

- exact task rejoin barriers;
- exact wait-consume transitions;
- an already owned activation lane;
- explicit operator or control-plane transitions with fixed placement; or
- no legal placement.

#### Static baseline

Preserve current queue-head, priority, runnable WorkItem, active WorkItem,
barrier, and liveness rules.

#### Rejection and fallback

Reject placement outside the allowed set, placement that skips a barrier, or a
proposal based on stale lane or queue facts.

### `InterjectionPlacementHook`

#### Purpose

Preserve the fixed trusted-operator priority lane and identify the
deterministically earliest legal provider/tool safe boundary at which an
already persisted interjection may be observed.

#### Input

- persisted message identity and authority;
- current turn, provider round, and tool-execution phase;
- safe points defined by the interjection RFC;
- the fixed `Priority::Interject` lane;
- the singleton allowed boundary containing the earliest legal safe point;
- whether an existing barrier or terminal transition is in progress; and
- the static interjection baseline.

#### Output

- `(Priority::Interject, earliest_legal_safe_boundary)`; or
- `Abstain`.

There is no `Drop` outcome for trusted operator input. The hook does not alter
the current provider request, splice text into an executing model context, or
cancel tool calls whose execution boundary is already authoritative.

The hook cannot choose a later boundary. Deterministic runtime code derives the
earliest legal safe point before invocation and exposes no alternative lane or
later safe point as a candidate. `Abstain` uses that same fixed pair through the
static baseline.

#### Invocation point

After the operator input is durably recorded and classified, before the
scheduler or turn loop selects the next legal interjection safe point.

#### Bypass conditions

- no active turn;
- exact control transition with a fixed safe point;
- terminal settlement already committing;
- input that is not an operator interjection; or
- trust or permission rejection.

#### Static baseline

Preserve current `Priority::Interject` ordering and the safe-point behavior
defined by the interjection RFC, including visibility after pending tools
complete and before the next provider decision.

#### Rejection and fallback

Reject a proposal that drops trusted input, crosses a forbidden tool boundary,
targets any boundary other than the current earliest legal safe point, changes
the fixed priority lane or message authority, or references a stale turn or
provider-round identity.

### `ContinuationDispositionHook`

#### Purpose

Choose the static post-activation posture only when no authoritative explicit
intent already determines settlement.

The existing `resolve_continuation` / `ContinuationResolution` path authorizes
message reentry from a concrete trigger. It is not this hook. This hook owns
only the later scheduling-posture choice represented today by
`decide_next_action` and the idle continuation path.

#### Input

- activation and turn terminal evidence;
- current WorkItem focus, execution owner, and scheduling posture;
- runnable work and queue facts;
- active waits, tasks, timers, and external triggers;
- legal continuation dispositions;
- relevant revisions and generations; and
- the static continuation baseline.

#### Output

One allowed disposition, such as:

- continue autonomously;
- yield to already runnable work;
- remain idle without autonomous continuation;
- request operator clarification when that disposition is already supported by
  the current protocol;
- follow an explicit handoff target already present in canonical facts; or
- `Abstain`.

A hook cannot create a wait condition, complete a WorkItem, invent a handoff
target, publish delivery, or synthesize terminal evidence. If a future enum
uses names such as `Wait`, `Complete`, `Clarify`, or `Handoff`, each variant
must require the corresponding authoritative evidence and remain subject to
the runtime gate.

#### Invocation point

After model/tool execution has produced terminal evidence and before the
runtime selects the next autonomous scheduling posture. Settlement commit
remains a separate deterministic transition.

#### Bypass conditions

- explicit `WaitFor`;
- explicit WorkItem completion;
- exact task rejoin;
- exact wait resume;
- explicit operator input;
- runtime error;
- terminal delivery or settlement recovery; or
- any authoritative continuation frame or control-plane instruction.

#### Static baseline

Preserve current `decide_next_action`, closure precedence, runnable-work,
active-work, wait, and liveness behavior.

#### Rejection and fallback

Reject unsupported terminal claims, invented wait or handoff identities,
dispositions lacking required evidence, or proposals based on stale activation
or WorkItem revisions.

### `SemanticCandidateSelectionHook`

#### Purpose

Reduce or order multiple candidates that are already legal and lack an exact
authoritative identity.

#### Input

- a typed selection purpose;
- two or more legal candidate references;
- bounded semantic facts required for that purpose;
- deterministic ordering keys;
- candidate revisions or generations; and
- the static selection baseline.

#### Output

- one candidate;
- an ordered subset with no duplicates;
- no selection; or
- `Abstain`.

The output cannot add a candidate, alter candidate facts, or convert an
ineligible candidate into an eligible one.

#### Invocation point

Within an owning decision point after hard filtering and before that decision
point chooses its final proposal. The initial Phase 1 implementation uses
static ordering only.

This is a shared sub-decision, not a second terminal scheduler. For example,
ingress routing may use semantic selection to order legal route candidates,
but `IngressRouteDispositionHook` still owns the route disposition and the
runtime gate still owns the transition.

In Phase 1 every owning hook constructs this sub-decision only when it has two
or more comparable legal candidates without an authoritative exact identity.
It invokes semantic selection at most once for that decision attempt, then
passes the returned candidate or ordered subset to its stage-specific static
disposition logic. The owning hook must not repeat semantic ranking or retain a
second copy of the migrated ordering rule.

This rule applies uniformly to ingress routing, wake eligibility, dispatch
placement, interjection, and continuation disposition. A hook whose
deterministic gate exposes zero or one legal candidate bypasses semantic
selection; therefore the Phase 1 interjection path normally bypasses it because
the runtime exposes only the earliest legal safe boundary. Semantic selection
is not limited to autonomous continuation.

#### Bypass conditions

- zero or one legal candidate;
- trusted exact identity;
- exact task, wait, activation, or WorkItem generation;
- candidates from different authority domains that cannot be compared; or
- a caller whose current contract requires fail-closed ambiguity.

#### Static baseline

Preserve existing priority, explicit focus, affinity, runnable queue, scenario,
and stable tie-breaking rules for the owning decision point.

#### Rejection and fallback

Reject unknown candidates, duplicates, candidates outside the input set,
illegal cross-domain ordering, or stale candidate identities.

This hook is the preferred first Phase 2 adapter because it can improve an
ambiguous choice while remaining unable to mutate runtime state.

## Hook Composition And Ordering

The five owning hooks and the shared `SemanticCandidateSelectionHook` form the
six-hook catalog, but they do not all run for every event. Semantic candidate
selection is a typed shared hook whose result is consumed by an owning hook;
it is not a second terminal scheduler.

A typical targetless ingress path is:

1. deterministic ingress normalization and trust classification;
2. deterministic legal route candidate construction;
3. semantic candidate selection exactly once when multiple comparable
   candidates remain;
4. ingress routing proposal;
5. runtime validation and durable queueing;
6. wake eligibility when the queued event matches a sleeping target;
7. dispatch placement before admission;
8. runtime claim, binding, execution, and settlement;
9. continuation disposition only when no authoritative intent bypasses it.

An operator interjection uses the interjection hook only after durable
recording and normally bypasses semantic selection because deterministic code
supplies one earliest legal boundary. Exact task rejoin and exact wait resume
bypass semantic selection and use their existing deterministic paths.

Implementations must not run hooks recursively or let two hooks produce
competing terminal actions for the same stage.

## Static Implementations

Phase 1 should provide one directly testable static implementation per hook.
It may call extracted pure helpers, but the implementation must preserve the
existing rules rather than re-specify them differently.

The preferred shape is:

- one function that constructs the hook context from already-read facts;
- one pure static implementation;
- one validation function;
- one wrapper that chooses proposal or baseline; and
- an existing runtime gate that applies the effective result.

The runtime should not retain both an old implicit choice and a new static hook
choice after migration. Once a decision point is connected, callers should use
the hook boundary and the static implementation should be the single
locatable expression of the old rule.

## Observability

Phase 1 may emit bounded structured diagnostics containing:

- hook kind;
- decision attempt or activation identity;
- `bypassed`, `proposed`, `abstained`, `rejected`, `fallback`, or `stale`;
- a closed reason code;
- selected candidate identity when that identity is already safe for the
  existing audit surface; and
- whether the effective result came from the proposal or baseline.

Diagnostics must not include free-text model reasoning, secrets, raw provider
payloads, or a new shadow-decision store. Existing durable transition evidence
remains authoritative.

## Characterization Requirements

Before extracting or connecting a decision point, tests must lock down the
existing behavior it will replace.

### Cross-hook invariants

- exact trusted identity bypasses semantic choice;
- candidates outside the deterministic legal set are rejected;
- `Abstain`, hook error, invalid proposal, and unavailable hook equal the
  static baseline;
- stale snapshot identity causes a fresh decision attempt, not a stale
  fallback commit;
- the hook cannot mutate storage or queue state;
- the runtime gate revalidates before commit; and
- default configuration requires no network or provider.

### Ingress routing

- exact agent and WorkItem affinity remains unchanged;
- existing targetless or ambiguous ingress remains fail closed;
- trust and permission rejection occurs before selection;
- a proposal cannot redirect across an authority boundary; and
- callback trigger ownership remains authoritative.

### Wake eligibility

- exact task result resumes only its captured owner and WorkItem;
- exact wait resume consumes only the matching wait generation;
- timer, external, operator, task, and local-continuation wake sources retain
  their current matching behavior;
- ambiguous canonical waits remain fail closed and observable; and
- deferred or ineligible wake does not consume the wait;
- a deferred durable wake remains pending until its recorded reevaluation
  condition occurs; and
- no hook result can silently discard a matched durable wake obligation.

### Dispatch placement

- interjection and task-rejoin queue precedence remains unchanged;
- queue barriers are not skipped;
- active lane ownership prevents a second admission;
- runnable and active WorkItem ordering remains unchanged; and
- no-candidate and no-placement cases retain current liveness behavior.

### Interjection

- trusted interjection remains durable and cannot be dropped;
- no interjection mutates an in-flight provider request;
- `Priority::Interject` remains fixed;
- the hook can return only the earliest legal provider/tool safe point;
- interjection remains visible after pending tools complete and before the next
  provider decision;
- terminal settlement is not interrupted after its commit boundary; and
- stale turn or provider-round proposals are rejected.

### Continuation disposition

- explicit `WaitFor`, completion, task rejoin, wait resume, and operator input
  bypass autonomous continuation choice;
- runtime errors and closure precedence remain unchanged;
- autonomous continuation suppression and duplicate protection remain
  unchanged;
- runnable work and active WorkItem continuation retain current ordering; and
- settlement and delivery remain deterministic.

### Semantic candidate selection

- zero and one candidate bypass selection;
- stable baseline ordering is preserved;
- ordered subsets contain no unknown candidates or duplicates;
- conflicting exact identities fail closed; and
- stale revisions or generations cause a fresh decision attempt;
- each owning decision attempt invokes selection at most once; and
- owning hooks do not duplicate the migrated semantic ordering rule.

## Initial Characterization Anchors

Step A locks down the existing static behavior without introducing hook test
doubles. The initial anchors are:

| Boundary | Characterization anchors |
| --- | --- |
| ingress routing | `ingress::tests::inbound_request_preserves_exact_routing_and_provenance_facts`; HTTP ingress rejection and provenance tests under `tests/support/http_ingress.rs`; `callback_capability_routes_only_to_owning_agent` and related callback capability tests under `tests/support/http_callback.rs` |
| wake eligibility | `wake_matching::tests::waiting_reason_matching_matrix_is_fail_closed`; exact task/wait ownership and canonical wait tests under `src/runtime/tests/contracts/wait.rs`, `src/runtime/tests/work_items.rs`, and `src/runtime/tests/runtime_state.rs` |
| dispatch placement | queue precedence and barrier tests in `scheduler_executor::tests`, including interjection, task-rejoin, and durable-wait ordering |
| interjection placement | interjection classification tests in `scheduler::tests`; durable visibility and terminal-boundary tests under `src/runtime/tests/runtime_state.rs` and `src/runtime/turn/tests.rs` |
| continuation disposition | reentry authorization tests in `continuation::tests`, kept distinct from idle-posture tests under `src/runtime/tests/scheduler.rs` |
| semantic candidate selection | `work_reactivation_prefers_runnable_current_over_queued_candidates` and `work_reactivation_uses_stable_fifo_order_for_queued_candidates` |

Proposal rejection, hook failure, unavailable-hook fallback, and stale proposal
tests begin in step B, when those typed primitives exist. Step A must not create
placeholder hooks solely to test behavior that the current runtime does not
yet expose.

## Implementation Sequence

This RFC supports the following Phase 1 sequence:

1. **A — contract and characterization**
   - accept this RFC;
   - add missing behavior-preservation tests.
2. **B — shared internal primitives**
   - add minimal snapshot identity, candidate reference, validation, abstention,
     and fallback helpers;
   - add the shared static semantic-selection implementation and invocation
     helper;
   - do not connect a provider or change outcomes.
3. **C — routing hooks**
   - connect `IngressRouteDispositionHook`;
   - connect semantic selection at the ingress call site when required.
4. **D — activation policy hooks**
   - connect `WakeEligibilityHook`;
   - connect `DispatchPlacementHook`;
   - connect semantic selection at activation call sites when required.
5. **E — execution-boundary hooks**
   - connect `InterjectionPlacementHook`;
   - connect `ContinuationDispositionHook`;
   - connect semantic selection at execution-boundary call sites when required.
6. **F — convergence audit**
   - remove bypasses around the new internal boundaries;
   - run cross-hook behavior-preservation tests;
   - document the narrow Phase 2 adapter seam.

Each step may be split into smaller pull requests. A later step must not weaken
an invariant accepted by an earlier step.

## Phase 2 Boundary

Phase 2 may define:

- a provider-agnostic request and response protocol;
- provider adapters;
- timeout, budget, cancellation, and retry behavior;
- shadow invocation and evidence;
- hook allowlists and authority policy; and
- the first non-static implementation.

Phase 2 must still use the Phase 1 legal candidate sets, validation, stale-fact
handling, and runtime gate. It should begin with semantic candidate selection
for autonomous WorkItem continuation rather than exact task rejoin, exact wait
resume, or explicit operator input.

## Decisions

1. **Ingress remains fail closed.** Phase 1 does not make previously rejected,
   targetless, or ambiguous ingress routable. Static implementations reproduce
   the existing baseline. Any new routing capability requires a later explicit
   protocol or policy change.
2. **Continuation uses posture-oriented vocabulary.** The proposal vocabulary
   uses names such as `ContinueAutonomously`, `YieldToRunnable`, `RemainIdle`,
   `RequestClarification`, and `FollowHandoff`. It does not expose bare `Wait`
   or `Complete` powers. Waiting and completion remain evidence-backed runtime
   transitions that bypass the hook.
3. **Diagnostics reuse the existing scheduler diagnostic audit surface.**
   Rejected, fallback, and stale outcomes are durable because they explain the
   effective execution path. Ordinary bypassed, proposed, and abstained
   outcomes use bounded metrics or logs by default. Diagnostics use closed,
   low-cardinality fields and never store free-text reasoning or create a
   shadow-decision store.
4. **Only hook traits carry the `Hook` suffix.** The ingress trait is
   `IngressRouteDispositionHook`, covering candidate selection plus allowed
   unresolved or deferred dispositions without implying transition ownership.
   The interjection trait is `InterjectionPlacementHook`, making its safe-point
   placement role explicit. Context, proposal, baseline, and static
   implementation types omit the `Hook` suffix.

## Acceptance Criteria

- all six hooks have explicit input, output, invocation, bypass, baseline,
  validation, and fallback contracts;
- static behavior is the only Phase 1 implementation;
- exact identity and hard state mutation remain in the deterministic gate;
- hook failure cannot make a candidate legal or commit stale facts;
- characterization requirements cover routing, wake, placement,
  interjection, continuation, selection, and settlement;
- no Phase 2 provider API or runtime dependency is introduced; and
- the decisions above are normative for Phase 1.
