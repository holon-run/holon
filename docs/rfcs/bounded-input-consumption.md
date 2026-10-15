---
title: "RFC: Bounded Input Consumption"
date: 2026-10-14
status: accepted
handle: rfc-bounded-input-consumption
---

# Bounded Input Consumption

This contract implements the approved P1/P2 direction of issue #3530. Acceptance
of the contract is not evidence that both implementation stages have shipped.
P1 and P2 require separate tests and pull requests. Waiting for future input uses
the existing `WaitFor` contract; there is no new `AwaitInputs` tool or P3 stage.

Related contracts:

- [Activation/Turn owner identity](activation-turn-owner-identity.md)
- [Unified execution protocol](scheduler-work-item-unified-execution-protocol.md)
- [Agent message routing](agent-message-routing-context.md)
- [Operator safe points](operator-interjection-safe-points.md)
- [Waiting and reactivation](waiting-plane-and-reactivation.md)

## Current structure and stages

The scheduler selects and durably claims a canonical queue anchor, then admits
eligible task-result settlements for that execution. Context construction
already renders multiple admitted results. Task-result notification messages
and settlement identities are not interchangeable.

P1 adds bounded initial input batches. Baseline tests must first count actual
activations, provider requests, and admitted/settled result identities; the
existing limit of eight settlements does not prove one activation per result.
Keep working result paths rather than introduce a second reducer.

P2 attaches eligible inputs to an already-running turn at a provider-request
safe point. It reuses the location and persistence pattern of operator
interjections, not their identity, trust, or authority.

## Grouping is not authority

Grouping means presenting several original inputs in one execution. It never
merges message identities, permissions, reply routes, or settlement obligations.
Do not infer grouping or authority from body text.

For an input with a runtime-validated recipient relationship, use:

1. an explicit recipient WorkItem owner;
2. otherwise, the source's explicit lifecycle or exact delivery scope;
3. otherwise, for an ordinary unbound agent message, its authenticated runtime
   sender and required source/session boundary.

An invalid or conflicting relationship must not fall back to a sender bucket.
The sending agent's WorkItem and the recipient's current focus do not establish
recipient ownership. In particular, the sender relation is not a transitive
bridge between owner A, an unbound message, and owner B.

Unbound ordinary messages from one sender may be presented together without
proving that their bodies describe one business task. Preserve receive order
and all originals: the model, not a runtime latest-only reducer, interprets
subsequent corrections and additions.

Task results and reply-wait settlements retain their canonical source-specific
admission. A legitimate cross-WorkItem waiter may receive its result without
changing the task's captured owner. A shared reply body is presented once, but
each waiter settlement remains an independently counted identity.

## P1: fixed, bounded initial batches

Select the anchor using existing canonical scheduling and control priorities.
Fix a source cutoff or candidate identity snapshot; scan at most 64 candidates.
Do not wait for future inputs or repeatedly drain a live queue to fill a batch.

The initial limits are eight source identities and 24 KiB of rendered UTF-8
input, including headers, provenance, route selectors, and source references.
The anchor and newly admitted result identities count toward the same limit;
the provider context budget may impose a smaller limit.

Reserve capacity before source admission. Do not admit eight results and later
hide some of them to make the context fit. Recovered, already-admitted inputs
must remain visibly recoverable before claiming additional ones.

Within a compatible, equal-priority mixed batch, reserve two places for eligible
task results and two for ordinary messages, then fill remaining capacity using
existing priority/FIFO ordering. A missing source releases its reservation.
Control/operator inputs retain their dedicated precedence.

Oversized single inputs must have a progress path: a bounded preview plus
recoverable original content and complete identity/routing metadata. Without a
recoverable original reference, reject admission rather than silently consume
truncated content.

## Persistence and per-input settlement

Reuse canonical queue transitions, execution authority, turn input identities,
incoming transcript evidence, and the task-result settlement ledger. Historical
`ActivationInputAttachment` wire records are not a new execution authority.

For a queue follower, source claim/consumption and durable turn attachment must
commit atomically. Persist each original input ID, its grouping evidence, reply
route, and presentation/source reference before the provider request.

Anchor claim and task-result admission already have separate commits. Preserve
their defined recovery sequence rather than claim a nonexistent cross-source
transaction. Queue consumption at durable attachment and result settlement at
execution terminal are distinct source contracts, not business completion.

Every commit retains execution, owner, source generation/revision, stop,
replacement, completion, and unique-consumption fences. Competing claimants
cannot both attach one input. Recovery reuses durable identities and effects;
it must not requeue an entire batch after a provider or tool effect.

Repeated interrupted replay preserves the original ingress turn stamp. Terminal
queue settlement resolves that stamp through a bounded durable replay chain;
each intermediate turn must retain the same agent, source message, and owner.
Missing intermediate turns, foreign bindings, and cyclic lineage fail closed.

Provider fallback carries input evidence from the complete bounded, validated
turn lineage in original order, deduplicated by message identity, without
readmitting sources or migrating their ownership or settlement identities.

## Replies and forwarding

`SendAgentMessage.reply_context_id` selects a runtime-provided, durably admitted
original message route in the current legal execution. It cannot supply sender,
correlation, authority, or execution ownership.

A unique inherited route remains implicit for backward compatibility. Multiple
eligible routes require an explicit selector for a response or forwarding;
omission is an actionable ambiguity error, not selection of the latest input.
An unrelated third-party send remains a new direct route. `forward=true`
requires a unique or explicitly selected source and preserves original reply
and relay semantics.

The route registry must be recoverable from durable admitted input evidence and
must reject unknown, foreign-execution, and expired selectors. Task results do
not manufacture agent-message reply authority.

## P2: eligibility and safe-point timing

For the same recipient, a task result whose runtime creation/rejoin records
prove `parent_turn_id == current_turn_id` is directly eligible, before any
owner/sender classification. This includes command, child, and existing-agent
reply-wait handles created in this turn; arbitrary later agent messages are not
all results of such handles.

Other exact task/reply/wait sources require canonical delivery eligibility.
Ordinary owner-bound inputs require the current execution owner. Unbound
ordinary messages may only extend a durably established matching sender batch;
a WorkItem turn does not acquire this privilege from its latest sender.

Attach only when no provider request is in flight and accepted tool calls have
their receipts paired. A background task's already-returned handle is a receipt;
the runtime does not wait for that task to finish.

Each safe point admits at most one fixed batch. New P2 attachments per turn are
bounded by four batches, 32 identities, and 96 KiB, additionally respecting
round/context limits. Prioritize eligible same-turn results within this budget.

Ordinary inputs only attach before an otherwise-required next provider request.
One exception mirrors operator interjection: after a pure final candidate and
before terminal commit, already-arrived, eligible same-turn task results may be
durably attached and cause another provider request. If none fit, do not wait or
invent a request. Unconsumed inputs retain their subsequent delivery path.

Terminal transitions and `WaitFor`/`CompleteWorkItem` pending-report stages stop
new attachment. Later results use existing continuation/reentry; they cannot
reopen the old turn or override its report binding.

### Live attachment transaction

P2 uses a separate durable live batch, never an enlarged idle-start batch.
Each batch records its current canonical ledger activation, immutable safe point
(original attachment activation and attempt-local target provider round), fixed
cutoff, exact settlement identities, source evidence, and rendered byte budget.
The source's same-parent grant is checked against the durable task's rejoin
fence, not an envelope's claimed turn or the current focus. Reply-wait results
retain the original reply body and independent grants for each waiter.

Source revalidation and admission, queue/delivery consumption, the agent-state
CAS, turn attachment, transcript, and audit commit atomically. A competing claim
or stale execution cannot leave an admitted result without its attachment.
New admission is bounded before claiming by both the live-batch budget and the
next request's context/round capacity. Already-admitted evidence is replayed from
the durable snapshot, not clipped by the idle pending-results display limit.
Recovery preserves the predecessor's source identities and budgets; it does not
grant new inputs merely because an old attempt has closed or disappeared.
Recovery rebinds the ledger activation atomically, but preserves each batch's
original safe point and all predecessor snapshots. New batches use the current
attempt's actual provider-round ordinal, not an offset from inherited rounds.
Both boundaries targeting the same request share one attempt-scoped safe point;
a recovered attempt's round two cannot collide with its predecessor's round two.
Context deduplication uses that immutable identity, and the four-batch, 32-identity,
96-KiB cumulative budget includes all inherited batches across every attempt.
Bootstrap closes the canonical attached turn with the interrupted queue/attempt
transition, not an ingress turn selected by its envelope ID. Retained queue
claims recover the validated interrupted lineage leaf rather than the original
attempt. Direct predecessor selection accepts canonical live or idle attachment;
invalid or ambiguous attachment cannot fall back to an ingress turn. Only a
predecessor without any attachment retains the older P1 envelope fallback.
An attached recovery ancestor requires an explicit interrupted turn terminal.
A legacy batchless ancestor may lack that terminal only when an interrupted
attempt and the exact persisted successor replay edge identify it; any existing
non-interrupted terminal remains ineligible.
This does not relax the existing bounded-replay policy for non-lifecycle
attempts. An already-admitted
notification may yield only to its own interrupted anchor after canonical
source validation; independent rejoin, operator, and exact-waiter barriers remain.
Initial result identities come from that activation's admission ledger, while
live identities also require membership in its persisted batch.
Recovery follows the persisted successor replay edge; claim-local envelope
annotations are not immutable source evidence.

## Ordinary wake hints and external events

Ordinary wake hints mean "check durable sources" and coalesce by recipient agent,
without requiring matching owner or sender. They are scheduling signals, not
business inputs. A new hint arriving during a claimed check must retain a path
to a subsequent check.

Actual business events remain in their source's durable queue/inbox with
independent claim/ack obligations. A callback capability or trigger ID identifies
an ingress, not necessarily an upstream sender. Do not guess an owner from
current focus or a payload-provided WorkItem ID.

Exact `WaitFor` obligations match and resume separately; an ordinary pending
check cannot replace their resource, generation, or consumption identity.
New durable external-event adapters, unknown-source event batching, and
webhook/timer attachment into a running turn are outside these implementation
stages. A contentful hint-only callback is not a lossless business-event queue.

## Acceptance evidence

Use existing Rust runtime/provider fixtures, not a synthetic queue simulation.
Compare identical traces before/after: message/result IDs, activations, provider
requests, settlement identities, pending high-water marks, and operator service.

Cover 1/2/8/9 messages and 1/2/8/9/16/17 results; corrections; owner/sender/source
boundaries; shared reply with multiple waiters; budgets; oversized inputs;
operator priority; duplicate/stale/replaced sources; concurrent claimants; and
recovery before/after attachment, provider/tool effects, and terminal settlement.

Report deterministic fixture gains only as measured fixture gains. Real-model
latency or production communication reductions require a separately authorized
experiment. Neither stage authorizes production deployment or PR self-merge.
