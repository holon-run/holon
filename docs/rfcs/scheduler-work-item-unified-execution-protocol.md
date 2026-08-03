---
title: RFC: Scheduler–WorkItem Unified Execution Protocol
date: 2026-08-01
status: accepted
handle: rfc-scheduler-work-item-unified-execution-protocol
---

# RFC: Scheduler–WorkItem Unified Execution Protocol

## Summary

Holon's canonical scheduler uses one execution protocol for queued input,
WorkItem continuation, wait resume, task rejoin, and lifecycle interaction.
The protocol keeps deterministic admission, generations, provenance, atomic
commit, and immutable execution evidence while removing scheduler-owned
mirrors of WorkItem and wait lifecycle.

One `ExecutionAttempt` grants one model-execution quantum. An attempt is the
only durable fact that owns the single agent lane. WorkItem, queue, wait, task,
host, and agent-control records remain authoritative for their own lifecycle.
Candidate construction is ephemeral. Admission and settlement are each one
business command and one SQLite transaction.

This RFC supersedes the control-state model in
[Agent Activation, Settlement, and Dispatch](./agent-activation-settlement-and-dispatch.md)
where activation slot, dispatch reservation, WorkDemand, scheduler wait
mirror, and missing-settlement recovery jointly decide future eligibility.
The earlier RFC's provenance, fencing, deterministic transition, idempotency,
atomicity, immutable evidence, and process-global cutover requirements remain
accepted.

## Motivation

The scheduler was introduced to replace case-by-case coordination among
messages, WorkItems, waits, tasks, turns, and restarts with one explicit
protocol. The current implementation achieved deterministic transitions and
strong persistence boundaries, but also copied business lifecycle into
`slot`, `dispatch`, `WorkDemand`, scheduler waits, activation state, and
missing-settlement recovery.

That duplication makes normal execution depend on agreement among several
authorities. A locally correct transition can leave a reserved lane, stale
focus, missing settlement, or mismatched wait owner that prevents unrelated
work from running. Adding more repair branches preserves the duplicated model
instead of restoring the original goal.

The target is not a third scheduler and not a return to implicit legacy
behavior. It is a smaller canonical kernel with one authority per question.

## Authority Matrix

| Question | Authority | Evidence or projection only |
| --- | --- | --- |
| Is an input queued, claimed, or terminal? | queue/message state | attempt source, Turn, audit |
| Is the agent executing? | one open `ExecutionAttempt` | agent status, historical slot |
| Is a WorkItem runnable, waiting, paused, or terminal? | `WorkItemExecutionState` | plan, todo, focus, WorkDemand |
| What can resume a wait? | `WaitCondition` identity, owner, generation, state | dispatch, closure reason |
| Is a task active or terminal? | task ledger | message presence, agent posture |
| May the host admit execution? | host runtime registry phase | loaded-runtime observation |
| May the agent administratively accept work? | agent control | focus, waits, settlement history |
| What is the collaboration focus? | durable WorkItem focus | execution ownership |
| Why did execution start and how did it end? | immutable attempt admission and outcome | future eligibility |

An authority added to admission must replace an existing authority in the
same change. No new durable candidate, reservation, intent, or lifecycle
mirror may be introduced without an explicit authority transfer.

## Core State

### ExecutionAttempt

```text
attempt_id
agent_id
source kind + identity + generation
owner/binding
origin + trust + priority + provenance
admitted fences
state = Open | Settled | Interrupted | ProtocolViolation
run_id / turn_id
recovery_of_attempt_id
terminal_outcome_id
admitted_at / terminal_at
```

Only `Open` owns the agent lane. All other states are terminal and lane-free.
The attempt records execution authority and evidence; it does not own future
WorkItem, queue, wait, or task lifecycle.

### WorkItemExecutionState

```text
Runnable(g)
InFlight(g, attempt_id)
Waiting(g, wait_id, wait_generation)
Paused(g, reason)
NeedsRepair(g, repair_id)
Terminal(g, completion)
```

Allowed transitions are:

```text
Runnable(g) -> InFlight(g, attempt)
Waiting(g, exact triggered wait) -> InFlight(g, attempt)

InFlight(g, attempt) -> Runnable(g + 1)
InFlight(g, attempt) -> Runnable(g + 1, recovery_ref)
InFlight(g, attempt) -> Waiting(g + 1, wait)
InFlight(g, attempt) -> Paused(g + 1, reason)
InFlight(g, attempt) -> NeedsRepair(g + 1, repair)
InFlight(g, attempt) -> Terminal(g + 1, completion)

Paused(g) | NeedsRepair(g) -> Runnable(g + 1)
```

An `InFlight` state and its same-agent open attempt must reference each other.
Creating a new eligibility epoch increments the scheduling generation.
Terminal transition resolves owned waits and continuation obligations and
removes scheduler eligibility. Focus may be cleared or restored in the same
transaction, but it does not grant eligibility.

### WaitCondition

The runtime wait record is the sole wait authority. Admission consumes one
exact triggered generation. Settlement may create or rearm one exact
generation. A scheduler-specific wait mirror or dispatch reservation must not
participate in admission.

### Candidate

`ActivationCandidate` is a pure, ephemeral value containing source identity,
generation, proposed binding, provenance, priority, and expected revisions.
It is neither persisted nor reserved. Unknown source/binding combinations
return `Unsupported` or `Quarantined`; they do not fall through to model
execution.

## Candidate And Outcome Matrix

| Source | Binding | Allowed result | Source transition |
| --- | --- | --- | --- |
| trusted operator input | interaction or exact WorkItem affinity | conversation or WorkItem outcome | exact claim to terminal |
| external contentful input | interaction or verified WorkItem binding | conversation or compatible WorkItem outcome | exact claim to terminal/quarantined |
| task result | exact live rejoin, unbound reduce-only, or stale | compatible outcome or `ReduceOnly` | consume exact result; stale does not reenter model |
| child result | exact caller continuation or unbound notification | caller outcome or `ReduceOnly` | consume exact result/continuation |
| triggered wait | exact wait owner | owner-compatible outcome | consume exact wait generation |
| WorkItem continuation | exact runnable generation | WorkItem outcome | `Runnable -> InFlight` |
| targeted yield/continuation | exact continuation frame | target-compatible outcome | consume exact continuation |
| internal contentful follow-up | declared interaction or WorkItem binding | binding-compatible outcome | exact claim to terminal |
| startup recovery, repair, stale reduction | command-owned, no model binding | typed `CommandResult` | deterministic state repair |
| shutdown or closed host | none | reject or defer | no execution |

Task and child results require a live rejoin obligation in addition to a
matching WorkItem generation. Priority orders candidates only after trust,
owner, generation, host, and agent-control gates pass.

## Admission

`AdmitExecution(candidate, expected_facts)` performs one transaction:

1. validate host and agent-control admission fences;
2. verify there is no open attempt for the agent;
3. compare-and-swap the source authority:
   - claim an exact queue entry;
   - move an exact WorkItem generation to `InFlight`;
   - consume an exact triggered wait;
   - bind an exact live task/child rejoin or continuation;
4. create the open attempt with provenance and admitted fences;
5. bind source, WorkItem, wait, task, Run, and audit references;
6. commit before provider execution begins.

There is no separate authority issuance, slot claim, dispatch reservation, or
WorkDemand reservation.

Trusted operator interjection attaches durable input evidence to an existing
attempt at an allowed boundary. It does not create a second open attempt.

## Settlement

`SettleExecution(attempt_id, outcome, expected_facts)` accepts an
owner-specific outcome:

```text
ConversationOutcome =
  Replied | Wait(wait) | Interrupted(reason) | Failed(policy)

WorkItemOutcome =
  Continue | Wait(wait) | Complete(completion) | Pause(reason)
  | Yield(target) | Failed(policy) | Interrupted(reason)
```

One transaction:

1. compare-and-swap the open attempt;
2. validate owner/outcome compatibility;
3. terminalize the source queue claim;
4. update WorkItem execution state;
5. consume, resolve, create, or transfer waits;
6. perform continuation or yield handoff;
7. bind Run and Turn terminal facts;
8. write immutable outcome and audit evidence;
9. write completion/final-delivery outbox records;
10. close the attempt and commit.

Terminal tools such as `WaitFor`, `CompleteWorkItem`, execution-bound
`PickWorkItem`, and explicit pause/fail/yield lower directly to settlement.
A terminal tool returns success only after the transaction commits and then
ends the provider/tool loop.

A WorkItem-bound provider turn that ends without a WorkItem outcome closes the
attempt as `ProtocolViolation` and normally returns the WorkItem to
`Runnable(g + 1, recovery_ref)`. Repeated violations without new durable
progress may move that WorkItem to `NeedsRepair`; they do not reserve the
agent lane.

## Interruption And Restart

Recovery is message/WorkItem-level model reentry, not provider or tool-call
continuation.

At startup, an open attempt without live host execution is recovered in one
transaction:

1. mark the old attempt `Interrupted`;
2. release its source claim or in-flight reservation;
3. create a new recovery eligibility epoch;
4. preserve prior transcript, tool, task, brief, WorkItem, and workspace
   evidence for the next model turn.

The scheduler does not infer whether an unrecorded external side effect
occurred. The recovery agent inspects current durable and external state,
decides whether work already happened, and verifies, continues, or repeats as
appropriate. The runtime never automatically replays an old tool-call
identity.

Source recovery is fixed by source kind:

| Interrupted source | Recovery transition |
| --- | --- |
| queued message | `Dequeued -> Interrupted`, same message remains claimable |
| autonomous WorkItem | `InFlight(g) -> Runnable(g + 1, recovery_ref)` |
| wait resume | release resume claim; preserve exact triggered obligation |
| task/child result | release rejoin claim; preserve exact live result identity |
| attached interjection | keep durable input evidence in recovery context |

A committed terminal outcome is never reentered into the model. Missing or
corrupt non-authoritative evidence may be diagnosed or rebuilt without
changing eligibility.

## Atomic Handoffs

Each of the following is one business transition, not a command sequence:

- lifecycle wait to WorkItem wait;
- task terminal result to exact WorkItem rejoin;
- child WorkItem completion to caller continuation;
- targeted yield from source WorkItem to target WorkItem;
- operator input attachment to a running attempt;
- orphan queue claim to restart recovery.

Each transition validates the source identity and generation, updates the
target lifecycle, closes or records attempt outcome when applicable, and
writes audit evidence in the same transaction.

## Conflict Containment

| Conflict | Runtime result |
| --- | --- |
| duplicate command, same payload | return the stored result |
| same identity, different payload | typed payload conflict; preserve first result |
| stale generation or trigger | no-op/stale evidence; do not mutate current state |
| temporarily inadmissible candidate | keep source eligible and try bounded alternatives |
| terminal or paused target | quarantine that candidate; continue unrelated work |
| owner or binding conflict | isolate the source/WorkItem; keep operator and unrelated work available |
| repeated protocol violation | move the affected WorkItem to `NeedsRepair` |
| corrupt primary partition | stop that partition; keep other agents and read APIs available |

A normal typed conflict must not cause an unbounded queue-head retry, agent
restart loop, or lane reservation leak.

## Persistence And Compatibility

The runtime database remains the transactional store. The target normalized
facts are:

- WorkItem execution state;
- runtime wait conditions and generations;
- queue claims and terminal status;
- tasks and rejoin obligations;
- execution attempts and immutable outcomes;
- continuation handoffs;
- command results, audit, and delivery outbox.

Historical activation authorities, slots, dispatch rows, scheduler WorkDemand,
scheduler wait mirrors, missing-settlement rows, and rollout metadata become
compatibility data or rebuildable evidence. During one compatibility release
they may receive derived writes, but the canonical reader must not consult
them for admission or settlement. Compatibility-write failure must not create
partial primary state.

The process chooses `legacy` or `canonical` once at startup. There is no
scenario, agent, conflict, or claim-time fallback between engines.

## Migration

1. characterize current behavior and production incident traces; publish this
   authority matrix and state algebra;
2. build the new aggregate and commands on isolated fixtures and copied
   databases without changing the production canonical reader;
3. complete candidate, admission, settlement, interruption, handoff, and
   persistence-equivalence coverage;
4. switch canonical admission, settlement, and recovery together in one
   cutover;
5. remove compatibility readers and writes, split global invariants into
   aggregate and transaction-boundary invariants, then remove dead schema;
6. run required scheduler CI, crash/restart, soak, incident replay, upgrade,
   repair, and rollback drills;
7. publish a canonical-default compatibility release retaining the
   process-global legacy selector;
8. remove legacy only after one explicit observation period and operator
   approval.

Legacy removal is not authorized by this RFC's implementation alone.

## Required Verification

- pure transition sequence and property tests;
- SQLite rollback and reload equivalence;
- crash points at claim, admission, provider start, tool evidence, terminal
  transition, delivery, and commit;
- recovery of open attempts without automatic tool replay;
- candidate/binding/outcome/source-transition matrix;
- duplicate and out-of-order generation matrix;
- stale task result with no live rejoin obligation;
- wait and final-delivery consistency;
- host shutdown/admission linearization;
- queue-head conflict containment and restart-loop prevention;
- multi-agent SQLite busy and SIGKILL soak;
- deterministic replay of known scheduler incidents;
- compatibility database upgrade, diagnosis, repair, and rollback.

Acceptance requires both safety and simplification: one open attempt per
agent, one WorkItem generation per admission, one wait authority, atomic
terminal delivery, no persisted candidate or terminal intent, and no
slot/dispatch/WorkDemand/missing-settlement authority in the canonical reader.

## Non-goals

- multi-lane model execution;
- general resource scheduling or cross-agent assignment;
- semantic-model authority over admission;
- replacement of the provider turn loop;
- removal of append-only audit;
- making plan, todo, prose, or focus grant execution;
- deletion of legacy before the compatibility release and observation gate.
