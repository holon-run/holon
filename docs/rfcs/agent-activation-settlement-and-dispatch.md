---
title: RFC: Agent Activation, Settlement, and Dispatch
date: 2026-07-20
status: accepted
handle: rfc-agent-activation-settlement-and-dispatch
---

# RFC: Agent Activation, Settlement, and Dispatch

## Summary

Holon's scheduler must grant execution through a first-class
`AgentActivation` and close that execution through one
`ActivationSettlement`.

The model or agent may propose semantic binding, work disposition, waiting,
and future dispatch posture. The deterministic runtime remains authoritative
for:

- admission and claim;
- activation-slot ownership;
- WorkItem demand reservation;
- wait trigger and consume;
- revision and generation fences;
- trust, permission, lifecycle, and runtime constraints;
- atomic durable commit; and
- replay, recovery, and public projections.

This protocol separates four facts that the current runtime can accidentally
conflate:

1. durable WorkItem focus;
2. activation execution ownership;
3. semantic operator-input affinity; and
4. whether the agent lane accepts ordinary autonomous dispatch.

The migration is additive and gated. The current Message/Run/Turn scheduler
remains authoritative while activation records and decisions run in shadow.
Authority moves only after deterministic equivalence, persistence, restart,
fault-injection, and divergence gates pass.

## Status And Relationship To Existing RFCs

This RFC is the normative target for scheduler admission, activation binding,
terminal settlement, WorkItem dispatch generations, agent lane admission, and
wait trigger/consume generations.

It extends:

- [Runtime Scheduler Contract](./runtime-scheduler-contract.md);
- [Runtime Transition Commit Contract](./runtime-transition-commit-contract.md);
- [WorkItem Scheduling Read Model](./work-item-scheduling-read-model.md);
- [WorkItem Current Focus Canonical Fact](./work-item-current-focus.md);
- [Scheduler Wait State And Recoverable Agent Continuation](./scheduler-wait-state.md);
- [Work Item Runtime Model](./work-item-runtime-model.md); and
- [Work Item Centered Agent Runtime](./work-item-centered-agent-runtime.md).

Where those documents describe `SystemTick`, `plan_status`, generic blocker
text, global waiting posture, or current focus as scheduler authority, this
RFC defines the target replacement. Current implementation-facing behavior
remains documented under `docs/website/spec/` until each migration phase
becomes authoritative.

The Phase 0 normative conflicts are explicit:

| Earlier RFC statement | Status under this RFC |
| --- | --- |
| `scheduler-wait-state.md` derives `Runnable` from `plan_status=ready` and no `blocked_by` | Superseded for the target scheduler. Runnability requires an offered scheduling generation and the canonical activation, wait, yield, hold, and lifecycle gates. |
| `scheduler-wait-state.md` derives `WaitingOperator` from `plan_status=needs_input` | Superseded. Only an active operator `WaitCondition` is a waiting fact; planning posture remains metadata. |
| `scheduler-wait-state.md` treats generic `blocked_by` text as `Blocked` authority | Superseded for the target scheduler. The text is compatibility/display data; a typed manual hold or active wait supplies authority. |
| `work-item-runtime-model.md` derives queued or blocked scheduling views directly from `plan_status` or `blocked_by` | Superseded for the target scheduler. Those fields remain planning/display data and cannot create or suppress demand. |
| older scheduler paths use contentful `SystemTick` or current focus to select WorkItem execution | Compatibility behavior only. `AgentActivation` cause and binding plus fenced demand own admission and mutation authority. |

`scheduler-wait-state.md` is therefore partially superseded: its current
`WaitFor` tool and compatibility persistence behavior remain useful during
migration, while its earlier target scheduling derivation does not.

## Problem

Holon currently admits work through several partially overlapping paths:

- queued Message claim;
- provider Turn continuation;
- synthetic `SystemTick`;
- task-result rejoin;
- wait wakeup;
- WorkItem focus and readiness;
- continuation frames; and
- closure-derived follow-up.

Turn completion is also distributed. A final assistant message, WorkItem tool
call, wait registration, queue settlement, focus change, continuation update,
delivery promotion, and closure decision may occur in different steps.

This creates failure modes that local state normalization cannot solve:

- the same WorkItem generation can be admitted more than once;
- a stale wait trigger can resume a rearmed wait;
- a provider can stop after progress output without a WorkItem disposition;
- an operator input can be bound to the wrong waiting WorkItem;
- an active wait can incorrectly block unrelated runnable work;
- focus, turn binding, and scheduler ownership can drift;
- a WorkItem can complete without its completion report;
- queue settlement can survive while the corresponding WorkItem transition
  rolls back, or vice versa; and
- restart can repeat an activation whose durable terminal boundary was
  incomplete.

The root problem is missing protocol boundaries, not missing heuristics. The
runtime needs a single admission object, a single terminal settlement, and
explicit generations for every scheduler-sensitive offer and wait.

## Goals

- make one activation the unit of scheduler-granted execution;
- make every normal activation end in exactly one typed settlement;
- make WorkItem demand and agent intake explicit and independently fenced;
- preserve deterministic runtime authority over admission and mutation;
- make wait trigger, consume, resolve, and rearm auditable;
- separate durable focus from execution ownership and operator affinity;
- prevent semantic models from escalating authority;
- make all scheduler-sensitive transitions replayable from canonical facts;
- provide a staged shadow migration with an explicit rollback boundary; and
- retain a small public tool surface by lowering tools into the protocol.

## Non-goals

- do not replace the provider turn loop in the first phase;
- do not make an LLM or classifier the scheduler;
- do not require a separate process for semantic routing;
- do not introduce multi-lane concurrent execution for one agent;
- do not make plan text, todo text, assistant prose, or briefs authoritative;
- do not require every conceptual record to use a separate table initially;
- do not remove current queue, Run, Turn, closure, or `SystemTick`
  compatibility before the authoritative cutover; and
- do not couple the protocol to one provider, UI, or transport.

## Authority Boundary

The protocol has two decision planes.

### Semantic Decision Plane

The Semantic Decision Plane may propose:

- operator-input intent;
- activation binding;
- clarification;
- plan revision;
- assignment ranking; and
- execution policy hints.

A proposal must be typed, versioned, attributable, and rejectable. It may
carry candidate identifiers, candidate revisions, confidence, and evidence.

It must not:

- claim or create an activation;
- allocate a final task, activation, lease, or fence identity;
- mutate WorkItem, wait, focus, queue, or agent state;
- increase trust, permission, priority, budget, or capability;
- bypass a revision or generation check; or
- turn an ambiguous binding into an authoritative match.

### Deterministic Scheduler Kernel

The runtime kernel:

- constructs the candidate set from canonical facts;
- validates semantic proposals;
- applies trust, ownership, lifecycle, priority, and runtime policy;
- reserves demand and the single agent activation slot;
- consumes an exact wait generation when applicable;
- commits activation and settlement transitions;
- rejects stale or conflicting commands; and
- projects scheduler and operator-facing state.

`Unresolved` and clarification are valid safe outcomes. A wrong automatic
binding is a protocol failure, not a tolerable ranking error.

## Core Terms

### Agent

A durable actor identity with one model-execution lane.

### Activation

One scheduler-granted execution quantum. It owns temporary execution
authority, not the long-lived objective.

### WorkItem

A durable goal that may progress across many activations.

### Work Demand

A scheduler-sensitive offer that a specific WorkItem generation may be
activated.

### Agent Dispatch State

The agent's explicit terminal choice about whether its free lane accepts
ordinary autonomous work:

```text
Open
Awaiting(wait_id)
```

### Agent Lane State

A runtime projection used for admission:

```text
Unavailable(reason)
Running(activation_id)
Awaiting(wait_id)
Open
```

### Settlement

The single terminal commit for an activation.

## Canonical Facts And Projections

The target protocol keeps these authoritative facts separate:

- agent lifecycle;
- activation slot;
- agent dispatch state and revision;
- runtime and control constraints;
- Message and queue lifecycle;
- WorkItem lifecycle;
- WorkItem scheduling generation or dispatch intent;
- WorkItem continuation frames;
- WaitCondition lifecycle and generation;
- AgentActivation;
- ActivationSettlement;
- Turn terminal record;
- focus;
- manual hold;
- Task lifecycle;
- operator delivery and completion report binding;
- audit events and durable outbox records.

The following are projections, not admission authority:

- `AgentStatus` compatibility labels;
- `RuntimePosture`;
- `AgentSchedulingPosture`;
- WorkItem readiness;
- closure;
- candidate class;
- blocker display text; and
- TUI or HTTP status.

Projections must be rebuildable from canonical facts and must never be written
back as the cause of a transition.

## AgentActivation

The conceptual record is:

```rust
struct AgentActivation {
    id: ActivationId,
    agent_id: AgentId,
    state: ActivationState,
    cause: ActivationCause,
    binding: ActivationBinding,
    priority: ActivationPriority,
    preemption: PreemptionPolicy,
    source_revision: Option<u64>,
    idempotency_key: String,
    admitted_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    settled_at: Option<DateTime<Utc>>,
    run_id: Option<RunId>,
    turn_id: Option<TurnId>,
    settlement_id: Option<SettlementId>,
}
```

Initial states:

```text
Admitted
Running
Settled
Interrupted
Cancelled
SettlementMissing
```

Allowed transitions:

```text
Admitted -> Running -> Settled
                    -> Interrupted
                    -> SettlementMissing

Admitted -> Cancelled
```

One agent may have at most one `Running` activation. A terminal activation
cannot be claimed again.

### Activation Cause

The initial typed causes are:

```text
OperatorInput(message_id)
OperatorInterjection(message_id)
MessageIngress(message_id)
TaskRejoin(task_id, message_id)
WaitResume(wait_id, wait_generation, trigger_id)
WorkItemRunnable(work_item_id, scheduling_generation)
WorkItemRecheck(work_item_id, recheck_generation)
InternalFollowup(message_id)
RuntimeRecovery(recovery_id)
SettlementRecovery(activation_id)
```

Every cause preserves origin, trust, correlation, causation, and source
identity. Human-readable rendered instructions are not the cause.

### Activation Binding

```text
Unbound
WorkItem(work_item_id)
WaitOwner(wait_id, owner_ref)
Interaction(interaction_id)
Lifecycle(agent_id)
```

An unbound activation cannot mutate WorkItem lifecycle, scheduling, waits,
manual holds, or completion state. WorkItem mutation tools use activation
binding as their default authority, never durable focus alone.

Binding may be resolved from:

1. explicit trusted identifiers;
2. exact wait identity;
3. verified interaction affinity;
4. operator confirmation; or
5. a validated semantic proposal.

Recency, a single waiting candidate, current focus, or natural-language
similarity alone is not an authoritative binding.

## Work Demand And Agent Capacity

WorkItem runnability is represented by a stable scheduling generation:

```rust
struct WorkDispatchIntent {
    work_item_id: WorkItemId,
    scheduling_generation: u64,
    class: DispatchClass,
    mode: DispatchMode,
    priority: ActivationPriority,
    not_before: Option<DateTime<Utc>>,
    state: DispatchState,
}
```

```text
DispatchClass = Start | Continue | Resume | Retry | Recheck
DispatchMode  = Autonomous | OperatorBoundOnly | Manual
DispatchState = Offered | Reserved(activation_id) | Consumed | Cancelled
```

The first implementation may project this intent from canonical WorkItem,
wait, hold, and settlement facts. Stable identity, generation, and reservation
must still be durable and auditable.

Agent eligibility is derived from:

```text
runtime capacity
    lifecycle active
    activation slot free
    provider/runtime lane available

explicit dispatch state
    Open
    Awaiting(wait_id)

runtime/control constraints
    not stopped, draining, manually suspended, or budget blocked
```

A candidate is eligible only when both sides match:

```text
Eligible(work, agent) =
    demand is Offered
    && WorkItem facts permit the dispatch class
    && activation slot is free
    && agent dispatch state accepts the candidate
    && runtime/control constraints permit
    && priority and fairness policy permit
```

`Open` means only that the lane accepts a legal candidate. It does not mean a
candidate exists and is not equivalent to human-visible `Idle`.

`Awaiting(wait_id)` reserves the free lane against ordinary autonomous
WorkItem dispatch. Exact resume of that wait, trusted operator intervention,
lifecycle control, and reducer-only transitions remain admissible.

## Admission And Claim

The scheduler consumes one `SchedulerProjection` and `CandidateSet` and emits:

```text
Stop
InjectInterjection
ClaimActivation(candidate)
ReduceCandidate(candidate)
StayIdle
Sleep(until)
```

`EmitSystemTick` is a migration executor, not a target WorkItem scheduling
decision.

Claiming WorkItem demand atomically:

1. validates the WorkItem scheduling generation;
2. validates the agent dispatch-state revision;
3. verifies that no activation slot is running;
4. reserves the offered demand for the new activation;
5. creates the `Admitted` activation; and
6. reserves the agent activation slot.

Claiming `WaitResume` additionally:

1. validates the exact wait and trigger generations;
2. changes the wait from `Triggered` to `Consumed`;
3. binds the activation to the wait owner;
4. changes matching `Awaiting(wait_id)` dispatch state to `Open`; and
5. reserves the activation slot in the same transaction.

No other work may enter between wait consume and activation-slot reservation.

Starting the activation changes `Admitted` to `Running` and claims the
corresponding queue entry, Turn binding, or compatibility Run record in the
same invariant-preserving transition.

## Activation Settlement

Every normally terminal activation commits one settlement:

```rust
struct ActivationSettlement {
    id: SettlementId,
    activation_id: ActivationId,
    turn_terminal: Option<TurnTerminal>,
    disposition: ActivationDisposition,
    agent_dispatch: AgentDispatchDisposition,
    operator_delivery: Option<DeliveryRef>,
    evidence: Vec<EvidenceRef>,
    created_at: DateTime<Utc>,
}
```

```text
ActivationDisposition =
    ConversationReplied
  | WorkContinues
  | WorkWaits(wait_spec)
  | WorkCompleted(completion)
  | WorkPaused(reason)
  | WorkYielded(target, mode)
  | WorkFailed(failure_policy)
  | ReducedOnly
  | Interrupted(reason)

AgentDispatchDisposition =
    Open
  | Awaiting(wait_id)
```

All terminal tools lower into the same runtime settlement command:

- `WaitFor`;
- `CompleteWorkItem`;
- `PickWorkItem`;
- terminal provider completion;
- interruption;
- reducer-only completion; and
- explicit continuation or pause.

A WorkItem-bound activation must include one WorkItem disposition.

### Missing Settlement

If a WorkItem-bound model Turn reaches normal provider terminal state without
a disposition:

```text
Activation -> SettlementMissing
WorkItem   -> NeedsSettlement(activation_id)
```

The runtime may admit at most one `SettlementRecovery` activation. Recovery
chooses a disposition from existing evidence and must not repeat external side
effects. A second failure moves the WorkItem to a typed system hold for
operator review.

The runtime must not infer Continue, Wait, Complete, or lane posture from:

- final prose;
- progress prose;
- todo state;
- plan status;
- previous closure;
- previous wait; or
- whether an assistant message exists.

### Delivery Boundary

```text
Progress
Final
CompletionReport
```

`Progress` is non-terminal. `Final` may close an unbound conversation but does
not replace a WorkItem disposition. `CompletionReport` is valid only when
bound to the successful WorkItem completion transaction.

## WorkItem Scheduling

Target WorkItem scheduling states are projections:

```text
Executing(activation_id)
Runnable(scheduling_generation)
Waiting(wait_ids)
Paused(hold_id)
Yielded(frame_id)
NeedsSettlement(activation_id)
Closed(outcome)
```

Durable focus is a separate facet.

A WorkItem scheduling generation may be offered only when:

- lifecycle is open;
- no activation for the WorkItem is running;
- no active or triggered-unconsumed wait blocks this generation;
- no active yield frame suspends it;
- no manual hold pauses it; and
- the latest settlement or explicit command produced an offered generation.

These facts do not directly determine runnability:

- `plan_status`;
- todo contents;
- plan text;
- generic `blocked_by` text;
- assistant text;
- brief contents; and
- ordinary WorkItem metadata revision.

The WorkItem metadata revision and scheduling generation are distinct. Plan
or todo edits do not create autonomous execution demand.

## Focus, Execution, And Interaction

Three relations are independent:

### Durable Focus

Durable focus is one canonical single-valued relation from agent to open
WorkItem. During legacy and shadow operation,
`agent_states.current_work_item_id` remains the production-authoritative
storage location. Phase 2 imports the same fact into
`scheduler_agent_focus`; once a scenario is authoritative, that normalized row
is the protocol authority and `agent_states.current_work_item_id` is an
atomically dual-written compatibility projection. The guarded cutover requires
the two locations to agree and rollback reverses the projection direction
without inventing a focus value.

Ordinary turn completion and WorkItem-scoped waiting do not implicitly clear
focus. Completion, explicit focus replacement, or continuation policy may
change it.

### Execution Ownership

`AgentActivation.binding` owns WorkItem mutation during the activation.
Changing durable focus does not rebind an already admitted activation.

### Operator Interaction Affinity

Exact wait identity or a verified interaction token may bind operator input.
An ordinary unbound operator prompt does not inherit current focus and does
not guess among waiting WorkItems.

This satisfies sticky discussion flows without allowing accidental mutation:
focus may remain on a waiting WorkItem while its execution ownership is
released and the open lane runs other work.

## WaitCondition Lifecycle

Every semantic wait has an owner:

```text
WorkItem(work_item_id)
Invocation(invocation_id)
Interaction(interaction_id)
AgentLifecycle(agent_id, reason)
```

Ordinary conversation waits use an interaction owner, not an unscoped generic
agent wait.

Wait states:

```text
Active
Triggered
Consumed
Resolved
Cancelled
Expired
```

Transitions:

```text
Active --matching event--> Triggered
Triggered --activation claim--> Consumed
Consumed --condition ended--> Resolved
Consumed --still waiting--> Resolved + Active(next generation)
Active/Triggered --owner close or replace--> Cancelled
Active --expiry--> Expired
```

Trigger, wait, and generation identities are fenced independently. A reusable
callback capability does not make one semantic wait reusable.

External and operator events trigger waits but do not resolve them. The
consuming activation must resolve, cancel, or rearm. A matching runtime-owned
terminal task result may perform trigger, consume, and resolve in one
transaction while retaining all logical audit facts.

`recheck_after_ms` is a fallback wake source for the same wait generation, not
a separate generic blocker timer.

## Atomic Commit Boundary

The restricted transition repository gains activation-specific commands:

```text
AdmitActivationCommand
ClaimActivationCommand
SettleActivationCommand
TriggerWaitCommand
```

The settlement transaction writes every durable fact changed by the
disposition, including as applicable:

- Turn terminal;
- activation terminal;
- WorkItem disposition and next scheduling generation;
- wait resolve, cancel, or rearm;
- agent dispatch state;
- activation-slot release;
- focus and turn-binding changes;
- continuation-frame transition;
- queue settlement;
- completion intent and completion report binding;
- audit events; and
- durable index or delivery outbox entries.

Only post-commit projections, process-local events, metrics, logs, and wake
notifications occur after the database commit.

Expected-state conflicts are business conflicts and are not retried as
transient SQLite failures.

## Phase 2 Persistence Layout

The existing runtime database remains the only authoritative transactional
store. `runtime_db::transitions` already owns `BEGIN IMMEDIATE`, revision
checks, audit and index-outbox writes, rollback, and post-commit effects.
`RuntimeStore`, scheduler read models, `AgentState.status`, and WorkItem
readiness remain adapters or rebuildable projections. Phase 2 extends that
transaction domain; it does not introduce a second event store or make a
serialized protocol `Snapshot` authoritative.

The additive schema separates current fenced state from immutable protocol
records:

- `scheduler_agent_slots`: one row per agent, containing either the idle slot
  or the exact running activation identity, WorkItem, admitted generation, and
  optional recovery target;
- `scheduler_agent_dispatch`: one row per agent containing `Open` or the exact
  awaited wait identity plus the monotonically increasing dispatch revision;
- `scheduler_agent_focus`: one row per agent containing the nullable focused
  WorkItem and a monotonically increasing focus revision; an explicit row with
  a null target means initialized with no focus;
- `scheduler_work_demands`: one row per `(agent_id, work_item_id)` containing
  metadata revision, scheduling generation, current protocol status,
  capabilities, locks, locality, and cost class;
- `scheduler_waits`: one row per `(agent_id, wait_id)` containing owner and
  current generation;
- `scheduler_wait_generations`: one immutable-identity row per
  `(agent_id, wait_id, generation)`, with lifecycle state, trigger identity,
  and consuming activation;
- `scheduler_activation_authorities`: one row per
  `(agent_id, authority_id)`, uniquely bound to the full activation identity
  and recording its one allowed consumer;
- `scheduler_activations`: one row per `(agent_id, activation_id)`, including
  the authority, canonical admission fence, cause, binding, provenance,
  lifecycle state, admitted WorkItem generation, and optional recovery target;
- `scheduler_activation_settlements`: one immutable settlement record per
  `(agent_id, settlement_id)`, with a uniqueness fence on the same-agent
  activation identity;
- `scheduler_missing_settlements`: the canonical recovery requirement for an
  agent activation that left execution without a valid settlement;
- `scheduler_continuation_admissions`: the immutable completion-to-caller
  generation transition record, with both WorkItems in the same agent
  partition;
- `scheduler_protocol_command_results`: one immutable first-seen command
  result per canonical command identity, including command kind, versioned
  payload hash, decision, typed conflict or rejection, result fact
  references, and pre- and post-state fences;
- `scheduler_protocol_command_conflict_attempts`: one immutable audit row per
  same-identity/different-payload attempt, including the agent or global
  rollout partition, both payload hashes, and the typed payload conflict,
  without replacing the first-seen command result;
- `scheduler_protocol_migrations`: one immutable import result per migration
  identity and legacy source identity, including migration version,
  provenance, payload hash, decision, typed rejection, and imported fact
  references; and
- rollout preflight, manifest, scenario-authority, and hard-blocker records
  required before any scenario becomes authoritative.

Every agent-local protocol row has a non-null `agent_id`. Primary keys and
foreign keys carry that partition even when an identifier is globally unique.
Authorities, activations, settlements, missing-settlement records, waits,
continuations, focus, and WorkItem demands cannot reference a row from another
agent. Rollout configuration and approved manifests may remain global or
tenant-scoped immutable inputs, but they are never discovered by scanning
another agent's protocol facts.

Foreign keys and unique indexes reinforce, but do not replace, reducer
validation. Required database constraints include:

- at most one running slot per agent;
- at most one admitted activation for an authority;
- at most one consumer for an authority;
- one focus row per agent, whose non-null target is an open same-agent demand;
- for ordinary `Scheduling` and `WaitResume`, one shared admission fence per
  `(agent_id, work_item_id, scheduling_generation)`;
- for `SettlementRecovery`, one admission fence per
  `(agent_id, work_item_id, scheduling_generation, missing_activation_id)`;
- at most one settlement for an activation;
- one wait-generation row for each `(agent_id, wait_id, generation)`; and
- a dispatch `Awaiting` identity that names a persisted wait generation owned
  by the same agent.

The database stores the reducer's canonical admission-fence value or an
equivalent pair of partial unique indexes. Admission cause is descriptive data
and is not part of the ordinary uniqueness key: `Scheduling` and `WaitResume`
must collide for the same WorkItem generation. Only settlement recovery adds
the missing activation identity, exactly matching `admission_fence` in the
protocol kernel.

`WorkDispatchIntent` is not a second authoritative table in Phase 2. It is a
typed proposal or command input. Once validated, its authoritative result is
the `scheduler_work_demands` generation and status transition plus the audit
record. Persisting both an intent row and a demand row as independently mutable
authority would create an avoidable reconciliation problem.

### Restricted Transition Repository

The activation-specific repository always receives an `agent_id`. Inside the
same database transaction it reconstructs exactly one agent's minimum protocol
`Snapshot` from `scheduler_agent_focus` and rows carrying that partition,
combines only the applicable immutable rollout inputs, executes the pure
reducer, calls `assert_invariants`, and persists the resulting row diff before
commit. It never scans unpartitioned WorkItem demands and never consults a
legacy WorkItem or `AgentState` projection to fill a missing canonical field.
An absent required partition row is corruption or incomplete migration, not an
empty default.

The first command surface is:

```text
IssueActivationAuthorityCommand
AdmitActivationCommand
SettleActivationCommand
RecordMissingSettlementCommand
TriggerWaitCommand
```

Each command type has a stable canonical identity:

- authority issuance uses `authority_id`;
- admission uses `activation_id`, with activation idempotency key retained as
  an additional uniqueness fence;
- settlement uses `settlement_id`;
- missing-settlement recording uses the missing-settlement record identity;
  and
- wait trigger uses `(wait_id, wait_generation)`; trigger identity and
  generation are payload, so a different trigger for the same wait generation
  is a payload conflict rather than a second command.

Before evaluating mutable state, the repository looks up
`scheduler_protocol_command_results` by `(agent_id, command_kind,
command_identity)`. The payload hash is computed from a versioned canonical
encoding, never from ad hoc JSON field order. Canonicalization first decodes
the declared wire version into the typed command, rejects unknown or ambiguous
fields, expands accepted aliases and defaults, validates the resulting typed
shape, and then encodes that normalized command with its canonical schema
version for hashing. Semantically equivalent accepted wire payloads therefore
produce one hash; a default, alias, or field-order variation cannot manufacture
a second command meaning. An equal hash returns the stored canonical result
without rerunning the reducer. A different hash returns a typed identity or
payload conflict against the immutable first-seen command and records the
conflicting attempt in audit evidence without replacing that result.

For a new command, the reducer decision and command-result row commit in the
same transaction as all produced protocol facts. The result row is also
committed for deterministic business rejection, including stale revision,
stale generation, stale authority, invalid binding, duplicate identity, and
unsupported transition. Its bounded outcome envelope stores the original
decision, conflict, transition or result references, and state fences; it is
not an authoritative serialized `Snapshot`. A caller that needs current state
performs a separate projection read.

SQLite busy, lock, process loss, or commit failure before a durable
command-result row may use the existing transient retry path. Once a result row
exists, the command is never re-evaluated against newer state. This prevents a
stale command rejected before restart from becoming successful after restart,
and prevents successful replay from duplicating audit, delivery, consume, or
outbox facts.

The transaction writes protocol facts, required legacy compatibility facts,
audit events, completion or delivery intents, and durable outbox entries
together. Scheduler notification, in-memory `AgentState` refresh, metrics,
logs, and index notification remain post-commit effects. A post-commit effect
failure therefore cannot invalidate the commit and recovery must rebuild those
effects from canonical rows and outboxes.

### Migration And Recovery

The schema migration is additive and creates no production authority. Legacy
WorkItem, wait, queue, Turn, and agent-state rows remain readable and writable
until the corresponding scenario class completes guarded cutover and rollback
drills.

Legacy import uses an explicit, versioned migration command with original
provenance. `scheduler_protocol_migrations` is keyed by both migration identity
and `(agent_id, source_kind, source_id)` so one legacy source cannot be
silently imported twice under a new command identity. Its versioned payload
hash and canonical outcome are written in the same transaction as imported
facts, including rejected outcomes. Equivalent replay returns the stored
result; a changed version, payload, source ownership, or provenance is a typed
conflict that requires an explicit repair or superseding migration rather than
reinterpretation.

The import allocates or records scheduling and wait generations once and
creates the agent slot, dispatch, and focus rows required for an independently
rebuildable partition. It rejects ambiguous legacy shapes rather than
inferring authority from plan text, readiness, blocker display text,
`AgentStatus`, or a synthetic tick. An existing explicit zero generation is
canonical data and is never treated as a missing legacy value.

Restart reconstructs each agent independently from its normalized partition,
validates the full snapshot at the deserialization boundary, and then derives
compatibility projections. A missing focus row, cross-agent reference, or
partition invariant failure blocks that scenario from authority; restart never
falls back to `work_items.agent_id`,
`agent_states.current_work_item_id`, or another legacy projection to repair the
snapshot.

Restart never replays provider turns or tool calls merely because an
activation is incomplete. A running activation without a durable terminal
settlement becomes a canonical missing-settlement recovery candidate; a
consumed wait retains its exact consuming activation and cannot be re-consumed
after restart. Command and migration result ledgers are loaded before accepting
new commands so previously rejected stale commands and successful commands
retain their original outcome.

An optional serialized snapshot may be stored only as a versioned,
checksummed recovery cache. Canonical rows remain the source of truth, and the
cache must be discarded and rebuilt when its schema version, checksum, or
invariant validation fails. Its cache envelope and typed decoder must
distinguish an omitted required field from an explicitly present nullable
value. In particular, a missing focus field is invalid while an explicit null
means initialized with no focus; the cache path must not reuse legacy serde
defaults that collapse those states.

## Completion Boundary

Successful WorkItem completion requires:

- matching activation binding;
- open WorkItem lifecycle;
- valid expected revisions and scheduling generation;
- a non-empty completion report candidate;
- todo acceptance or explicit override policy;
- owner waits resolved, cancelled, or transferred;
- active tasks terminal, detached, or transferred;
- deterministic continuation resolution; and
- permission to publish the completion result.

The target write set atomically commits:

- `WorkCompleted`;
- WorkItem terminal lifecycle;
- completion report and result brief binding;
- activation and Turn terminal;
- wait and hold cleanup;
- focus release when required;
- continuation resume or cancellation;
- resumed caller scheduling generation;
- queue settlement; and
- audit/outbox facts.

If implementation needs an internal `Completing` intent to stage the report,
that state is canonical and recoverable. The public lifecycle does not need to
expose it initially.

## Required Invariants

### Activation

1. One agent has at most one running activation.
2. One activation has at most one terminal settlement.
3. A terminal activation cannot be claimed again.
4. Operator interjection does not create concurrent model execution.
5. Every activation cause retains provenance and a stable idempotency key.

### Binding And Focus

6. Focus and activation binding are independent.
7. Unbound activations cannot mutate WorkItem-owned facts.
8. Mutation authority comes from activation binding, not focus.
9. Queued WorkItem execution does not silently change focus.
10. A completed WorkItem cannot be focused or newly bound.

### WorkItem Demand

11. One scheduling generation is reserved by at most one activation.
12. A WorkItem-bound activation has exactly one terminal disposition.
13. Waiting, yielded, paused, or settlement-missing work is not ordinarily
    runnable.
14. Plan status, todo, prose, and display blockers are not dispatch authority.
15. Scheduler-sensitive generation changes are independent of metadata edits.

### Wait

16. Every active wait has a valid owner and generation.
17. One trigger generation is consumed by at most one activation.
18. Wait resume binding identifies the exact wait generation.
19. Consumed waits do not remain waiting facts.
20. Consuming settlement resolves, cancels, or rearms the wait.
21. Rearm creates `Resolved(g) + Active(g+1)`; it does not mutate `g` back to
    active.

### Agent Lane

22. `Awaiting(wait_id)` references a live owned wait.
23. Only settlement or explicit runtime control writes agent dispatch state.
24. `Running` and `Unavailable` are runtime projections.
25. Display posture is never admission authority.

### Completion And Delivery

26. Completion and completion report commit atomically.
27. A failed completion transaction cannot publish success.
28. Progress output cannot terminally settle an activation.
29. A normal model terminal requires a terminal settlement.

### Replay

30. Candidate construction and admission are replayable from canonical facts.
31. Restart cannot duplicate a settled activation.
32. Stale revisions and generations are typed conflicts.
33. Equivalent command replay is idempotent and does not duplicate audit,
    delivery, or outbox facts.

## Migration

Migration proceeds through explicit authority modes:

```text
legacy
shadow
authoritative
```

The configuration surface is conceptually:

```text
scheduler.protocol_mode = legacy | shadow | authoritative
scheduler.scenario_mode.<scenario_class> = off | shadow | authoritative
scheduler.semantic_binding_mode = off | shadow | guarded
```

The implementation may choose different final key names, but it must expose
the same typed fields and state transitions. `protocol_mode` is the global
ceiling. A scenario class cannot exceed that ceiling, and entering global
`authoritative` mode does not implicitly authorize every class.

Every non-legacy deployment must persist a versioned rollout manifest with:

- the enabled scenario classes and each class mode;
- the protocol/schema build and fixture corpus revision;
- minimum shadow sample count and minimum consecutive observation duration;
- a maximum p99 latency-regression budget and the observed p99 regression for
  each scenario class;
- zero-valued safety and canonical-state divergence budgets, plus stable,
  reviewed observational divergence codes and their rates;
- required restart, fault-injection, and rollback evidence;
- a structured rollback trigger and action for each scenario class;
- the approver and approval timestamp; and
- a reference to the Runtime-owned successful preflight record consumed by
  this manifest installation.

The manifest author does not allocate a trusted preflight revision and cannot
self-declare a successful observation. The Runtime opens a canonical preflight
record for one proposed manifest revision, allocates its monotonically
increasing preflight revision, and later completes that same record from the
preflight executor's evidence. Installation requires exact equality between
the proposed manifest and the completed record's captured manifest, then
atomically marks the record consumed. An open, missing, mismatched, or already
consumed record is not installable.

Every replacement manifest revision invalidates prior preflight evidence and
starts a new observation window, including when all visible manifest fields
are otherwise unchanged. Configuration parsing must reject an authoritative
class without a matching successful manifest and consumed canonical
preflight record.

### Phase 0: RFC And Executable Baseline

- maintain the deterministic MVP reducer and fixture corpus;
- freeze the accepted terms, invariants, gates, and rollback boundary;
- retain the explicit supersession table in this RFC and matching notices in
  affected older RFCs; and
- keep production behavior unchanged.

Exit gate:

- scheduler and intent MVP fixtures pass;
- wait rearm uses real reducer transitions;
- stale generation and ambiguous binding are covered;
- missing settlement produces canonical
  `ActivationState::SettlementMissing + NeedsSettlement`;
- all scenarios assert full-state fixture equality; and
- the RFC receives independent review.

### Phase 1: Deterministic Protocol Kernel

- extract typed activation, settlement, demand, lane, wait, and validator
  types from the executable baseline;
- keep the kernel pure and storage-independent;
- add state-space, property, and conflict tests; and
- grant no production scheduler authority.

Exit gate:

- all required invariants are executable;
- no missing or ambiguous disposition is silently normalized;
- stale command rejection is deterministic; and
- the kernel has no dependency on provider prose.

### Phase 2: Additive Persistence

- add activation, settlement, dispatch, and wait-generation durable facts;
- implement restricted transactional commands;
- add restart, replay, migration, and fault-injection tests;
- preserve legacy reads and writes needed for rollback; and
- avoid destructive schema removal.

Exit gate:

- every commit-phase failure fully rolls back;
- every post-commit failure rebuilds from canonical facts;
- restart never duplicates activation or wait consume; and
- schema migration and rollback preflight pass on representative databases.

### Phase 3: Production Shadow

- the legacy scheduler remains authoritative;
- every legacy admission and terminal boundary produces a shadow candidate,
  activation, and settlement;
- compare binding, admission, wait consume, WorkItem disposition, queue
  settlement, and resulting projections; and
- persist structured divergence evidence.

Hard blockers:

- any wrong automatic WorkItem binding;
- duplicate activation or demand reservation;
- acceptance of a stale settlement or wait generation;
- unmatched queue terminal state;
- irrecoverable projection divergence; or
- shadow behavior that would grant broader authority than legacy behavior.

Phase 3 exit gate:

- every scenario class proposed for authority has complete structured
  comparison records for all admission and terminal boundaries;
- the observation window satisfies the class minimums below;
- there are zero hard-blocker events, zero unexplained divergences, and zero
  missing comparison records during the complete window;
- reviewed expected differences are identified by stable divergence codes,
  bounded by the rollout manifest, and cannot weaken a safety invariant;
- restart, replay, commit-failure, post-commit rebuild, and class-specific
  fault-injection suites pass against the candidate build;
- a rollback drill proves that legacy authority can resume without losing or
  duplicating admitted work; and
- the stored preflight confirms schema compatibility, legacy read/write
  availability, no running activation in the switching class, no unresolved
  consumed wait, no pending settlement recovery, and no incomplete outbox
  publication.

### Phase 4: Guarded Authority

Move one scenario class at a time:

1. reducer-only candidates;
2. exact task rejoin;
3. exact wait resume;
4. explicitly bound operator input;
5. WorkItem autonomous continuation;
6. ordinary semantic operator binding.

Each class has its own gate and rollback switch. Semantic binding remains
`off` or `shadow` until structural and explicit paths are stable.

Minimum complete shadow windows are:

| Scenario class | Minimum samples | Minimum consecutive duration |
| --- | ---: | ---: |
| reducer-only candidates | 10,000 | 72 hours |
| exact task rejoin | 1,000 | 7 days |
| exact wait resume | 1,000 | 7 days |
| explicitly bound operator input | 1,000 | 7 days |
| WorkItem autonomous continuation | 2,000 | 14 days |
| ordinary semantic operator binding | 5,000 | 14 days |

Both the sample and duration threshold are required. A deployment that cannot
produce the minimum representative traffic remains in shadow; it may satisfy
the sample requirement with a recorded production corpus replay only when the
replay includes the same canonical facts and injected failure boundaries as
live comparison. Time duration cannot be replaced by replay.

The executable baseline caps an approved p99 latency regression at 1,000 basis
points (10%). Each class records both that approved maximum and the observed
regression; the observed value cannot exceed the approved maximum.

Mandatory class evidence is:

| Scenario class | Additional required evidence before authority |
| --- | --- |
| reducer-only candidates | deterministic replay and duplicate command idempotency |
| exact task rejoin | duplicate/out-of-order task results and restart before rejoin settlement |
| exact wait resume | duplicate trigger, stale generation, restart after consume, and rearm |
| explicitly bound operator input | duplicate ingress, stale binding revision, and wrong-agent target |
| WorkItem autonomous continuation | concurrent claim, reservation conflict, yield return, and rollback |
| ordinary semantic operator binding | ambiguous/low-confidence input, conflicting proposals, and zero wrong automatic bindings |

For all classes, safety and canonical-state divergence allowance is exactly
zero. A non-zero allowance may cover only reviewed observational differences
such as diagnostic wording or ordering that does not affect binding,
admission, mutation, wait consumption, settlement, delivery, outbox effects,
or resulting authoritative projections. Each allowance is identified by a
stable code, records its reviewer, and is capped at 100 basis points (1%) in
the executable baseline.

An individual class may transition:

```text
off -> shadow -> authoritative
authoritative -> shadow -> off
```

`shadow -> authoritative` is a compare-and-swap command fenced by rollout
manifest revision, current configuration revision, and successful preflight
revision. The command must atomically record the class, evidence window,
threshold results, unresolved-divergence count, rollback target, and operator
authorization. Failure of any precondition leaves authority unchanged.

Every replacement rollout manifest revision opens a new preflight observation
window, even when its visible fields are otherwise unchanged. The Runtime,
rather than the manifest author, allocates the canonical preflight revision
and records the target manifest revision before observation begins. A
preflight record transitions only:

```text
open -> completed -> consumed
```

Completion captures the exact manifest observed by preflight. Installation is
a compare-and-swap transition that requires a successful preflight result, the
record to be `completed`, the record's target revision to equal the installed
manifest revision, and the captured manifest to equal the installation input.
A failed result cannot transition the canonical record to `completed` and
cannot be installed even if a persisted or migrated record incorrectly claims
that state. The same transaction marks the record `consumed`, so neither an old
result nor a replay of the completed installation input can authorize another
manifest. This binds build/schema identity, gates, observations, evidence,
divergence allowances, and rollback policy as one authority-sensitive input
rather than trusting self-declared revision fields or a partial
field-difference check.

Each authoritative class records the structured trigger
`any_hard_blocker` and action `stop_admissions_and_revert(target)`. Reporting a
hard blocker is a fenced reducer command that atomically records the blocker,
stops new protocol admissions for that class, and returns it to `shadow` or
`off` according to the recorded target; a separate manual rollback command is
not the authority mechanism. In-flight commands complete only under their
original authority and revision fences; they are never reinterpreted under the
new mode.

The blocker is an append-only authoritative fact containing the scenario
class, stable blocker code, configuration/manifest/preflight revisions, and
the trigger/action that caused rollback. It survives snapshot serialization
and later re-authorization. Every class mode (`off`, `shadow`, or
`authoritative`) must reject a rollback action whose target is
`authoritative`.

### Phase 5: Semantic Proposal Providers

- structural resolution remains first;
- semantic providers submit the common proposal envelope;
- low-confidence or conflicting proposals return `Unresolved`;
- wrong binding remains a zero-tolerance blocker; and
- local distilled models are interchangeable proposal providers, not runtime
  authorities.

Acceptance thresholds for model quality, latency, corpus coverage, and
hardware cost are maintained by the Semantic Decision Plane project. This
RFC fixes the safety boundary regardless of provider.

### Phase 6: Legacy Removal

Only after authoritative operation and rollback drills pass:

- remove WorkItem scheduling through contentful synthetic `SystemTick`;
- stop deriving waiting from `plan_status` or blocker prose;
- stop using compatibility status or closure as admission authority;
- remove obsolete dual-write paths and schema;
- update current implementation specs; and
- retain replay compatibility for historical records.

## Rollback

Rollback is a mode transition, not deletion of canonical history.

Before destructive cleanup, `authoritative -> shadow` must be possible when:

- no activation is running, or running activations have been interrupted and
  durably settled;
- every reserved WorkItem demand is consumed, cancelled, or reconstructed as
  legacy runnable work;
- every consumed wait has a recovery activation or is safely rearmed;
- queue and Turn terminal facts are reconciled; and
- the legacy executor can rebuild its projection from committed facts.

Rollback must not:

- reuse or decrement activation, scheduling, or wait generations;
- reactivate a consumed wait generation;
- delete activation or settlement audit history;
- reinterpret semantic proposals as commands; or
- publish a completion result that did not commit.

After destructive Phase 6 cleanup, rollback means deploying a database-aware
compatibility release, not flipping the runtime mode.

## Observability

Every scheduler decision and transition records:

- agent, activation, WorkItem, wait, task, Message, Run, and Turn identifiers
  when applicable;
- cause and binding;
- candidate class and rejection reason;
- expected and committed revisions or generations;
- proposal resolver and evidence;
- authority mode;
- legacy/shadow comparison result;
- correlation and causation;
- commit identity; and
- post-commit warning state.

Required diagnostics include:

- active or settlement-missing activations;
- lane state and its source facts;
- offered and reserved WorkItem generations;
- active, triggered, and consumed waits;
- orphaned `Awaiting(wait_id)` state;
- shadow divergences;
- stale command conflicts;
- incomplete rollback prerequisites; and
- completion intent without terminal publication.

## Verification

Each implementation phase must retain:

- `cargo fmt --all -- --check`;
- `RUSTFLAGS="-D warnings" cargo check --all-targets`;
- deterministic scheduler and intent fixture tests;
- transition conflict and idempotency tests;
- restart and replay tests;
- commit and post-commit fault injection;
- ambiguous binding tests;
- single-slot and duplicate-claim concurrency tests;
- stale scheduling and wait-generation tests;
- completion atomicity tests;
- shadow divergence tests; and
- rollback drills.

Performance testing must measure candidate construction, admission transaction
latency, settlement transaction latency, restart rebuild time, and shadow
overhead without weakening correctness gates.

## Deferred Decisions

The protocol deliberately defers:

- whether interaction affinity becomes a separate persisted record initially;
- whether public WorkItem lifecycle exposes `Completing` or `Failed`;
- final configuration key spelling and storage representation, but not the
  rollout manifest fields, authority states, or transition preconditions;
- multi-agent assignment policy;
- multi-lane execution for one agent; and
- which semantic proposal provider is preferred.

These choices may change representation or policy. They must not change the
authority boundary, generation fences, single-slot rule, atomic settlement, or
safe unresolved behavior defined here.
