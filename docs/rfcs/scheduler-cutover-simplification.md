---
title: RFC: Scheduler Cutover Simplification
date: 2026-07-31
status: accepted
handle: rfc-scheduler-cutover-simplification
---

# RFC: Scheduler Cutover Simplification

## Summary

Holon will keep the canonical scheduler protocol and remove the runtime rollout
control plane that was used to introduce it.

The canonical reducer, typed facts, atomic SQLite transitions, append-only
ledger, replay, generation fences, and fail-closed ambiguity handling remain
the scheduler foundation. Runtime shadow comparison, rollout manifests,
preflight revisions, per-scenario authority, automatic hard-blocker rollback,
and semantic proposal routing are not part of the long-term production
contract.

During one bounded compatibility window, a process-wide startup selector may
choose either the legacy or canonical scheduler. It has two values, defaults to
`canonical`, never mixes engines within one process, and must be deleted with
the legacy engine after the compatibility window.

Scheduler qualification evidence belongs in CI and release acceptance. Runtime
databases store scheduling facts, not approval evidence for the binary that is
currently running.

## Decision

The migration model is:

```text
CI and release environments qualify one build
                    |
                    v
        operator makes one release decision
                    |
                    v
 process starts with legacy or canonical selected
                    |
                    v
       exactly one scheduler owns all admissions
                    |
                    v
 legacy and the temporary selector are deleted
```

Holon will not maintain a production path that:

- runs legacy and canonical admission for the same input;
- compares internal outcomes at every scheduler boundary;
- grants authority independently by scenario class;
- requires a runtime manifest or preflight revision before ordinary work can
  run;
- automatically changes scheduler authority in response to a divergence; or
- lets a semantic model or provider participate in production admission.

## Relationship To Existing Scheduler RFCs

This RFC supersedes the migration and rollout policy in
[Agent Activation, Settlement, and Dispatch](./agent-activation-settlement-and-dispatch.md).
That RFC remains normative for canonical activation, settlement, wait,
generation, replay, and atomicity semantics.

This RFC also supersedes implementation notes or website text that describes:

- `legacy -> shadow -> authoritative` as the production rollout path;
- `RolloutManifest` or runtime preflight as an authority prerequisite;
- per-scenario promotion or rollback;
- production semantic proposal routing; or
- long-lived legacy/canonical shadow comparison.

[Runtime Scheduler Contract](./runtime-scheduler-contract.md) remains the
broader scheduler vocabulary and projection contract.

## Current, Transition, And Target States

### Current state

As of July 31, 2026, startup exposes a temporary process-wide
`legacy | canonical` selector. The selected engine is immutable for the process
and the two engines do not shadow or compare each other in production.

Historical rollout, shadow-comparison, and semantic-decision tables remain in
the published migration chain, but production scheduler snapshots and
transactions do not load or write them. The rollout types, reducer, gates,
revision fences, repository/API, hidden command surface, and semantic module
export have been removed. `holon debug scheduler-recovery` retains a read-only
summary of historical rollout rows so operators can distinguish compatibility
data from canonical recovery candidates.

### Transition state

The transition removes runtime rollout metadata from scheduler authority before
performing destructive schema cleanup.

A temporary process-wide selector may then be introduced:

```text
runtime.scheduler = legacy | canonical
HOLON_SCHEDULER=legacy|canonical
```

The precedence is:

```text
environment override > persisted configuration > canonical default
```

The selector is:

- parsed once during process startup;
- immutable until process restart;
- global to the runtime, not per agent or scenario;
- restricted to `legacy` and `canonical`;
- rejected when its value is invalid; and
- prohibited from changing the meaning of already admitted work inside the
  running process.

The selector may be implemented only if the remaining legacy path can be
restored without reintroducing the deleted shadow and rollout systems. If that
requires rebuilding a second large scheduler, Holon will remain canonical-only
and use binary/database rollback instead.

### Target state

The target runtime has:

- one canonical scheduler engine;
- no scheduler engine selector;
- no legacy admission path;
- no runtime rollout authority repository;
- no production shadow comparison;
- no production semantic proposal dependency;
- no manifest, preflight, scenario authority, or hard-blocker gate in ordinary
  scheduler transactions; and
- no schema whose rows can change scheduler authority at runtime.

Historical tables may remain unread for one compatibility release before a
later destructive migration removes them.

## Preserved Canonical Contract

Simplifying cutover must not weaken the scheduler protocol. The following
remain required:

- one activation owns one granted execution quantum;
- every activation reaches one durable terminal settlement or an explicit
  recovery state;
- queue, WorkItem, wait, activation, settlement, transcript, audit, and
  delivery effects that form one scheduler boundary commit atomically;
- WorkItem, wait, claim, and continuation generations reject stale work;
- duplicate and out-of-order inputs are idempotent or fail closed;
- durable facts, rather than `AgentState.status`, prose, or display state, own
  scheduling;
- restart and replay reproduce the same externally visible outcome; and
- ambiguous ownership never falls through to an implicit legacy decision.

The simplification removes transition machinery, not safety invariants.

## Engine Isolation

If the temporary selector is implemented, the engine choice occurs at the
highest scheduler admission/execution boundary. Deep transition commands must
not carry mode flags.

The two engines may share:

- message envelopes;
- WorkItem, task, and wait persistence;
- terminal queue contracts;
- transcripts, briefs, delivery, and audit records; and
- end-to-end scenarios and externally visible assertions.

They must not, for the same production input:

- both decide admission;
- both reserve or consume scheduler ownership;
- both write activation or settlement facts;
- generate a shadow candidate;
- compare internal dispositions; or
- fall through from canonical to legacy after canonical has claimed the input.

Comparison belongs in tests that run separate processes and databases. Tests
compare user-visible behavior and durable invariants, not byte-for-byte
internal records.

## Adoption Boundary

Switching engines is an offline startup operation:

1. stop the runtime;
2. create a database backup;
3. select the target engine;
4. perform one bounded, transactional, idempotent adoption;
5. start serving only after adoption and invariant checks succeed.

Adoption may reconstruct only business state needed by the selected engine,
including:

- open WorkItem scheduling generations;
- current focus and execution ownership;
- active wait generations;
- dequeued but non-terminal queue claims; and
- recoverable task rejoins.

Adoption must not install a rollout manifest, collect approval evidence, or
grant authority by scenario class.

## Scheduler Diagnose And Repair

Holon must provide an operator-facing scheduler diagnose/repair surface before
removing the last emergency fallback.

The repair contract is:

- diagnosis is read-only by default;
- every mutating operation supports dry-run;
- a mutating operation requires a verified backup or creates one before the
  transaction;
- repair actions are finite, typed, versioned, and idempotent;
- arbitrary SQL is not exposed as a repair action;
- normal startup and repair share the same business invariant checks;
- every mutation records an audit event with the selected action and affected
  identities;
- an unsafe or ambiguous repair fails closed and explains why it refused; and
- stale rollout metadata is diagnosed as retired compatibility data, not
  repaired into a new authoritative revision.

Initial diagnosis must cover:

- open WorkItem demand and generation;
- current focus and execution ownership;
- active wait identity and generation;
- queue claims without terminal disposition;
- recoverable task rejoin;
- activation without settlement;
- settlement without required delivery disposition; and
- projection drift that can be rebuilt from canonical facts.

The repair tool must call the same domain transitions used by the runtime. It
must not edit scheduler tables directly.

If canonical facts cannot be safely projected back to legacy, the supported
recovery is the pre-cutover database backup and previous binary. Holon does not
promise lossless hot rollback.

## Release Evidence

Scheduler qualification is bound to:

```text
git SHA + schema revision + image digest + fixture corpus revision
```

The release gate includes:

- formatter and warning-free compilation;
- reducer and property tests;
- migration and representative upgrade tests;
- restart, replay, duplicate, and out-of-order tests;
- SQLite transaction fault injection;
- Tier-1 and Tier-2 scheduler Docker E2E;
- Tier-3 concurrent, crash, provider-degradation, and external-trigger stress;
- database backup and rollback drills; and
- a machine-readable acceptance report.

The report is a CI or release artifact. It is not imported into runtime tables,
and agents do not validate it while starting or scheduling work.

Production observation after cutover uses ordinary error rate, latency,
duplicate-activation, stuck-wait, missing-settlement, and data-integrity
signals. It does not require a legacy implementation to generate divergence
records.

## Semantic Proposal Plane

The semantic proposal plane is outside the production scheduler scope.

The production dependency, module export, and scheduler authority class are
removed. Any future offline benchmark or experiment:

- cannot propose or grant production admission;
- cannot add scheduler authority classes;
- cannot block deterministic structural binding; and
- cannot require runtime scheduler persistence.

Reintroducing semantic routing requires a separate RFC after the canonical
lifecycle is stable.

## Compatibility And Schema

Published migrations are immutable. The transition therefore:

1. adds a later migration or code path that retires rollout authority reads;
2. stops loading rollout state into production scheduler snapshots;
3. stops passing rollout expectations through ordinary transactions;
4. tolerates historical authoritative rows with missing or stale evidence;
5. leaves historical tables intact for at least one compatibility release; and
6. drops them only in a later, independently reversible schema change.

The database stores scheduler facts and audit history. Configuration selects a
temporary engine. Neither historical rollout rows nor repair records select
authority.

## Rollback

There is no automatic per-scenario rollback.

During the compatibility window, supported rollback is:

1. stop new admission;
2. settle or explicitly interrupt in-flight activation;
3. stop the runtime;
4. run diagnosis and any safe typed repair;
5. restart with the other engine if reverse projection is supported.

When reverse projection is not supported, restore the pre-cutover backup and
run the previous binary.

Every supported path must be exercised in release acceptance. A rollback path
that has not been tested is not a supported recovery promise.

## Delivery Plan And Exit Criteria

Implementation is split into independently reversible changes:

1. publish this contract and mark superseded rollout text;
2. retire rollout metadata from production startup and transactions, while
   adding the safe diagnose/repair skeleton;
3. inventory the surviving legacy implementation and add the temporary global
   selector only if it remains small and isolated;
4. move qualification evidence into CI/release acceptance;
5. delete dead rollout, shadow, and semantic production surfaces (**complete**);
6. reduce activation types only after rollout authority is gone; and
7. delete legacy and the selector after one compatibility release.

The final legacy removal requires:

- canonical release gates passing continuously;
- representative upgrade, restart, fault-injection, and rollback drills;
- Tier-1 and Tier-2 without blockers;
- Tier-3 release acceptance;
- no unexplained data loss, duplicate activation, stuck wait, or missing
  settlement; and
- explicit operator approval for the removal release.

## Non-goals

- Do not replace the canonical scheduler with the legacy implementation.
- Do not weaken activation, settlement, wait, replay, or transaction
  invariants.
- Do not support live engine switching.
- Do not support per-agent or per-scenario engine selection.
- Do not preserve shadow comparison as a permanent observability system.
- Do not make the repair tool a general database editor.
- Do not combine destructive schema cleanup with the startup compatibility
  fix.
