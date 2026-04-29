# Result Closure Contract

Date: 2026-04-06

Related issue:

- `holon-run/holon#44` `RFC: result closure contract`

This RFC answers a basic runtime question:

How should `Holon` decide that the current work is closed, and what state
should the system enter after that closure?

The focus is not a specific workflow or prompt style. The focus is the runtime
contract.

## Problem

Today, `Holon`'s "done / continue / wait" semantics are still spread across
multiple places:

- prompt wording
- text-only assistant rounds
- `Sleep`
- task creation / task result
- callback / waiting intent
- timer
- operator follow-up

This creates several problems:

- the same runtime situation can be interpreted differently in `run` and
  `serve`
- the boundary between "completed" and "waiting for future change" is not
  stable enough
- the operator cannot always tell from status or transcript what the agent is
  actually waiting for
- workflow-specific symptoms can be mistaken for product-mode problems instead
  of closure-model problems

`Holon` needs one explicit result-closure contract.

## Goal

This RFC answers only two questions:

1. Has the current unit of work closed?
2. After closure, does the system terminate or enter a waiting state?

The unit of work here spans at least three levels:

- `turn`
- `task`
- `agent`

## Non-Goals

This RFC does not define:

- prompt wording
- verification policy
- review-fix workflow
- worktree policy details
- approval UX

Those may depend on the closure contract, but they should not define it.

## Core Judgments

### 1. Closure outcome must be decided by the runtime

The agent may provide an intent hint, but it must not be the final source of
truth for the closure outcome.

Why:

- the agent may claim "done" while the runtime still sees active tasks, pending
  waits, or an unclosed objective
- the agent may claim "waiting for external change" while the runtime can see
  that it is actually waiting for an operator decision

So:

- the runtime is the source of truth for closure outcome
- the agent only provides closure intent hints

### 2. `sleeping` should not be a semantic closure outcome

`sleeping` is better modeled as a runtime posture:

- no active turn is running
- the runtime is suspended waiting for a future trigger

It should not directly represent business meaning.

An agent may be in a sleeping posture while:

- awaiting operator input
- awaiting external change
- awaiting task result
- awaiting timer

So `Holon` should distinguish:

- semantic layer: why the system is waiting
- runtime posture: whether execution is currently suspended

### 3. `awaiting_operator_input` must exist explicitly

It should not be collapsed into:

- `awaiting_external_change`
- or generic `waiting`

Why:

- `awaiting_operator_input` means the current objective has reached a point
  where a human decision, clarification, approval, or additional input is
  required
- an ordinary external event should not substitute for operator input
- the trust and continuation semantics are different from external waiting

### 4. `awaiting_operator_input` should eventually require an explicit runtime signal

The runtime should not infer `awaiting_operator_input` from ordinary natural
language alone.

Why:

- asking a question in final text is not the same as entering an operator wait
  state
- relying on text interpretation would turn the runtime into a prompt parser
- operator wait has stronger continuation and trust semantics than a generic
  "I need more information" summary

So the staged recommendation is:

- in the short term, use only strong runtime evidence for
  `awaiting_operator_input`
- in the longer term, add an explicit runtime-facing primitive such as
  `WaitForOperator` or `RequestOperatorInput`

### 5. Creating a task is not sufficient to imply `awaiting_task_result`

`awaiting_task_result` should not mean "some task exists."

It should mean:

- the next meaningful progress on the current objective is blocked on that task
- and the parent has no remaining local work that should continue first

This matters because task creation can mean two different things:

- blocking delegation on the current critical path
- non-blocking delegated work while the parent continues locally

## Model

Use a two-layer model.

## Closure Outcome

The first layer should keep only three outcomes:

- `completed`
- `failed`
- `waiting`

Meaning:

- `completed`
  - the current unit has closed
  - it is no longer waiting for more progress within the same unit
- `failed`
  - the current unit has closed in explicit failure
  - it requires explicit recovery, retry, or new input
- `waiting`
  - the current unit has closed its current execution pass, but is explicitly
    waiting for a future trigger
  - this is not failure and not generic completion

## Waiting Reason

When `closure_outcome = waiting`, the runtime should also record a waiting
reason:

- `awaiting_operator_input`
- `awaiting_external_change`
- `awaiting_task_result`
- `awaiting_timer`

Meaning:

- `awaiting_operator_input`
  - the current objective requires human clarification, decision, approval, or
    missing input
- `awaiting_external_change`
  - the runtime still owns the objective, but must wait for a new external
    signal
- `awaiting_task_result`
  - the next progress on the current objective has been delegated to a task or
    child execution
- `awaiting_timer`
  - progress is blocked on time-based resumption

## Runtime Posture

Closure outcome and runtime posture should stay separate.

Current recommendation:

- `sleeping` is a runtime posture, not a closure outcome

That means an agent may be in states like:

- `waiting + awaiting_external_change + sleeping`
- `waiting + awaiting_task_result + sleeping`
- `waiting + awaiting_operator_input + sleeping`

Those share posture, but not meaning.

## Source Of Truth And Hints

## Runtime As Final Arbiter

The runtime should generate a closure record from observable system facts.

Observable facts include:

- whether a runtime or provider error occurred
- whether a clear final answer exists
- whether `Sleep` was called
- whether a task was created
- whether active waiting intents or callbacks exist
- whether active timers exist
- whether there is an explicit pending operator requirement
- whether the current objective has actually closed

## Agent As Intent Hint Provider

The agent may provide hints through:

- `Sleep(reason=...)`
- task creation
- callback / waiting registration
- future dedicated wait primitives

But those hints must not be treated as the final closure outcome by themselves.

The runtime still performs final derivation.

In particular:

- a free-form question in assistant text is not enough to force
  `awaiting_operator_input`
- task creation is not enough to force `awaiting_task_result`

## Outcome Derivation

At the end of a turn, the runtime should produce a closure record.

Conceptually:

```json
{
  "closure_outcome": "waiting",
  "waiting_reason": "awaiting_external_change",
  "runtime_posture": "sleeping",
  "evidence": [
    "sleep_called",
    "active_waiting_intent_exists",
    "no_pending_operator_requirement"
  ]
}
```

This does not mean the exact JSON structure must be persisted right now.

It means:

- the runtime should expose a closure decision
- the runtime should also preserve why that decision was taken

That is important for operator surfaces and debugging.

## Recommended Derivation Order

The first pass should use "explicit signals first, runtime facts as validation."

### 1. Check failure first

If there is:

- a runtime error
- a provider error
- a clearly terminal unrecoverable failure

then:

- `closure_outcome = failed`

### 2. Check explicit operator wait next

If the turn has reached a point where operator input is required, then:

- `closure_outcome = waiting`
- `waiting_reason = awaiting_operator_input`

Typical characteristics:

- there is not enough information to continue
- a human clarification or decision is required
- ordinary external triggers should not automatically replace that input
- there is strong runtime evidence, not only free-form assistant wording

### 3. Check task-result wait

If the next progress on the objective has been explicitly delegated to a task
or child execution, then:

- `closure_outcome = waiting`
- `waiting_reason = awaiting_task_result`

This should not be triggered merely because an active task exists.

It should require both:

- the task carries the next critical-path progress for the current objective
- the parent has no remaining local work that should continue before the task
  result arrives

### 4. Check external-change wait

If there is:

- an active waiting intent
- callback registration
- an external watch contract

then:

- `closure_outcome = waiting`
- `waiting_reason = awaiting_external_change`

### 5. Check timer wait

If the current state depends on time-based resumption, then:

- `closure_outcome = waiting`
- `waiting_reason = awaiting_timer`

### 6. Otherwise treat it as completed

If there is no waiting condition, no failure condition, and the current
objective is closed, then:

- `closure_outcome = completed`

## Meaning Across Turn, Task, And Agent

## Turn

Turn closure means:

- the current execution round has closed

It does not mean:

- the whole task is finished
- the whole agent lifecycle is finished

## Task

Task closure means:

- a bounded delegated unit has closed

Task closure usually re-enters the parent runtime through:

- `task_result`
- `task_status`

## Agent

For a long-lived agent, closure does not mean lifecycle termination.

It more accurately means:

- the agent's current active execution pass has closed
- the agent is now in some visible combination of closure outcome, waiting
  reason, and runtime posture

So the agent layer must not collapse:

- closure outcome
- waiting reason
- runtime posture

into a single field.

## Invariants

The first pass should keep at least these invariants:

1. A unit can have only one `closure_outcome` per closure decision.
2. `completed` and `failed` cannot both be true.
3. `waiting` is neither failure nor generic completion.
4. `waiting_reason` is meaningful only when `closure_outcome = waiting`.
5. `sleeping` is posture, not closure outcome.
6. The closure result must be explainable through operator-visible surfaces.

## Implementation Implications

This RFC should directly influence:

- `run` final-status mapping
- `serve` wake / sleep mapping
- task/result rejoin behavior
- delivery derivation
- status / transcript surfaces

But the first step is not a large rewrite.

Recommended order:

1. freeze the contract
2. define runtime-state mapping
3. add guardrail coverage

## `run` Final Status Mapping

`run` should gradually converge on the same closure vocabulary as the long-lived
runtime.

Recommended direction:

- `completed`
- `waiting`
- `failed`
- `max_turns_exceeded`

When `final_status = waiting`, `run` should also surface:

- `waiting_reason`

That means the current task-oriented status shape should evolve toward:

- `waiting + awaiting_task_result`
- `waiting + awaiting_external_change`
- `waiting + awaiting_operator_input`
- `waiting + awaiting_timer`

instead of continuing to treat one waiting subtype as a special product status.

In particular, `AwaitingTasks` should be understood as a transitional
implementation shape, not the long-term contract vocabulary.

## Open Questions

Remaining useful questions:

1. Should `awaiting_manual_resume` exist as a separate waiting reason?
2. What is the minimum acceptable runtime evidence for entering
   `awaiting_operator_input` before an explicit wait primitive exists?
3. How should the runtime represent the distinction between blocking and
   non-blocking delegation on the current objective?
4. Should `Holon` later add explicit agent-facing primitives such as:
   - `Complete`
   - `WaitForOperator`
   - `WaitForExternalChange`

## Decision

Current recommended direction:

- `Holon` should use `closure_outcome` to distinguish completion, failure, and
  waiting
- `Holon` should use `waiting_reason` to distinguish the semantic reason for
  waiting
- `sleeping` should remain a runtime posture rather than a business outcome
- final outcome must be decided by the runtime, while the agent only supplies
  intent hints
