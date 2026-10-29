---
title: RFC: Turn Model Lineage And Recovery
date: 2026-05-17
status: draft
---

# Turn Model Lineage And Recovery

This RFC defines how Holon should handle model switching, provider fallback,
provider continuation state, and automatic recovery across agent turns.

It extends, but does not replace:

- [Runtime Scheduler Contract](./runtime-scheduler-contract.md)
- [Turn-Local Context Compaction](./turn-local-context-compaction.md)
- [OpenAI Remote Compaction Boundary](./openai-remote-compaction.md)
- [Extensible Model And Provider Configuration](./extensible-model-provider-configuration.md)

## 1. Problem Shape

Holon turns can contain multiple provider/tool rounds:

1. build the effective prompt and tool surface
2. call a provider
3. receive assistant output and tool calls
4. execute tools
5. call the provider again with accumulated turn state
6. repeat until the turn reaches a terminal outcome

Provider fallback currently sits under the `AgentProvider` boundary. A failed
provider attempt can advance to the next configured candidate inside the same
`complete_turn` call. Even when that happens before any provider output is
accepted, it means the current turn may change model lineage, tool surface,
prompt cache shape, and provider continuation assumptions while still being
recorded as one logical turn.

The ambiguity matters because provider model changes are not just a different
text generator. They can change:

- tool schema shape, including freeform grammar versus JSON function tools
- provider-specific prompt-cache behavior
- native context-management policy
- OpenAI `previous_response_id` continuation state
- OpenAI remote compaction items with encrypted provider content
- retry and output-token behavior

If Holon switches model lineage in the middle of a logical turn, the next model
may be asked to continue from a conversation, tool surface, cache state, or
provider window that was produced for a different provider contract.

## 2. Goals

- keep one logical turn's provider contract stable for the whole turn
- preserve fallback by deferring cross-lineage fallback to a new turn
- make user-requested model switches predictable
- prevent encrypted remote compaction from crossing model lineage boundaries
- make provider recovery explicit and auditable
- avoid unpaired tool-call protocol state at turn boundaries
- keep durable runtime state provider-independent

## 3. Non-Goals

- do not remove provider fallback
- do not require every fallback candidate to expose the same native provider
  features
- do not make remote provider compaction a source of semantic memory
- do not replace turn-local compaction
- do not require an LLM summarization pass for recovery
- do not make the model the authority for whether a turn can be recovered

## 4. Terms

### Agent Turn

One runtime execution pass started from an external operator message, internal
message, timer, waiting-plane wake, or recovery message.

A turn may contain many provider/tool rounds. A turn owns the in-memory
provider conversation projection and reaches one terminal outcome.

### Provider Round

One provider request/response inside a turn. A provider round may produce text,
tool calls, token usage, diagnostics, and provider transport state.

### Model Lineage

The model-facing contract used for one provider round.

At minimum, a lineage includes:

- provider id
- model id
- transport contract
- endpoint kind
- tool schema lowering mode
- native provider feature set
- request controls such as reasoning effort and context-management policy

OpenAI continuation state should further include the request shape that guards
`previous_response_id` and provider-window replay.

### Continuation Scope

`ContinuationScopeId` is the provider-neutral identity used to find candidate
provider continuation state across rounds. The runtime assigns one stable scope
per agent and passes it explicitly on `ProviderTurnRequest`.

The scope is not a wire field, prompt-cache key, run id, turn id, or round id.
It only identifies the candidate lineage window. The transport must still
compare the full request shape before using incremental continuation.

### Turn Active Model

The first model lineage that successfully produces assistant/provider output
for a turn. After this is set, the turn is locked to that lineage.

### Side Effect Boundary

The point after which the turn has produced state that another model lineage
must not implicitly inherit inside the same turn.

The boundary is crossed when any of the following has happened:

- assistant text was accepted into the turn record
- a tool call was accepted into the turn record
- a tool result was produced
- a workspace or external side effect occurred
- provider continuation state was advanced
- provider remote compaction state was installed

### Recovery Turn

A new internal turn created after a locked model fails later in a turn. It
continues from durable Holon state rather than hidden provider state.

## 5. Proposed Semantics

### 5.1 User Model Switches Are Next-Turn Only

When an operator or configuration update changes the requested model while a
turn is running, Holon should record the requested change as pending. It should
not change the model lineage used by the active turn.

The pending model becomes eligible when the next agent turn is started.

Exception: if a turn has not yet sent a provider request, the new model may be
used for that turn because no provider state exists yet.

### 5.2 Current-Turn Retry Is Lineage-Local

A running turn may retry the same model lineage according to provider retry
classification.

This covers retryable failures such as:

- rate limits
- transient transport failures
- provider service failures
- completed provider responses that contain no wire content/output items

Retry must not advance to a different model lineage inside the same turn.
Cross-lineage fallback is a scheduling decision for the next turn, not a
continuation of the current turn.

A response is `empty_response + retryable` only when the wire response
successfully completes but contains no content/output items. A non-empty wire
response whose items are unknown or unsupported remains
`invalid_response + fail_fast`. Token usage reported by an empty response is
failure evidence and is included in the provider attempt timeline; it does not
change the classification.

### 5.3 Cross-Lineage Fallback Defers To The Next Turn

When the active lineage cannot complete a provider request after its retry
budget is exhausted and a fallback candidate exists, Holon should terminate the
current turn and queue a new internal turn that promotes the fallback candidate.

This rule applies even when the failure happened before the current turn
accepted assistant output. A pre-output fallback can feel seamless to the
operator, but it should still be represented as a new turn so the next model
gets a freshly built prompt, tool schema, provider request controls, cache
identity, and provider continuation state.

The terminal record for the failed turn should distinguish:

- `deferred_to_fallback` when no accepted assistant/tool state existed yet
- `provider_failed_needs_recovery` when the side effect boundary was crossed

The next turn should include enough runtime context to explain that model
selection was promoted because the previous lineage failed. It must not reuse
hidden provider state from the failed lineage.

### 5.4 First Successful Candidate Defines The Turn Lineage

Once a model lineage successfully returns provider output that Holon accepts
into the turn, Holon records that lineage as `turn_active_model_ref`.

All later provider rounds in the same turn must use that lineage. If it later
fails after retries are exhausted, the current turn terminates with an explicit
provider-failure terminal kind. It must not continue under another model
lineage.

The turn terminal record should include:

- requested model ref
- active model ref
- fallback target model ref, if one was queued
- failed provider attempt timeline
- whether the side effect boundary was crossed
- whether a recovery turn was queued
- last accepted assistant text preview, if any

### 5.5 Recovery Uses A New Turn

Holon may automatically enqueue an internal recovery or fallback message after a
lineage fails. The next turn is a normal new turn:

- it rebuilds the effective prompt
- it rebuilds the tool schema
- it applies pending model or fallback selection
- it starts with no prior provider `previous_response_id`
- it does not replay encrypted remote compaction from the failed lineage
- it continues from durable transcript, working memory, work-item state, and
  workspace state

The recovery/fallback message should tell the model that the previous turn
stopped because the active provider failed and that it must continue from
persisted state without assuming hidden provider context.

Suggested recovery context:

```text
Runtime recovery: the previous turn stopped after the active provider failed.
Continue the validated source task from persisted evidence and workspace state.
Current focus is not the authority for choosing the recovery task.
Do not assume hidden provider continuation state is still available. Do not
repeat completed tool work unless current evidence shows it is necessary.
```

Automatic fallback/recovery should be bounded. A repeated failure should stop
and surface the failure to the operator instead of creating an infinite recovery
loop. Holon must also prevent fallback loops where each new turn retries the
same failed primary model instead of promoting the next candidate.

#### Shared availability and request admission

Availability facts are shared within one runtime database, not by WorkItem
focus. Quota uses the existing account identity; transport has separate records
and cannot masquerade as an exhausted account. Connection scopes come from
configured endpoint/transport/proxy identity, while ambiguous or request/read
timeouts use the narrower route/model scope. Neither implies a host-wide breaker.

API-key transports without an account identity use a coarse hash of provider,
configured endpoint URL and resolved credential. Models sharing that credential
share admission, but another credential or endpoint does not inherit its quota
facts. This is not proof of account-wide equality; Codex keeps its existing
account identity. Raw credentials never enter incident or episode records.

When no quota identity exists (including legal `auth:none` routes), a 429
belongs to a separate route-throttle scope hashed from the configured provider,
endpoint, transport, proxy path and model route. It is neither an invented
account nor a transport outage. Credentialed routes retain their existing quota
scope instead. This supplemental scope does not change the legacy effective-chain
fingerprint: upgrading an unfinished root cannot reset its budget, cursor,
operator grant or deadline. Actual configuration changes retain existing chain
identity semantics.

HTTP 402 is a typed `payment_required` candidate failure and remains fail-fast:
it never retries the same HTTP candidate in the current request. Its independent
scope hashes the configured endpoint/transport/proxy path, model route, and
existing credential identity. It does not assert an account-wide quota failure
or make other models, credentials, or plan endpoints unavailable. This supplemental
scope also stays outside the legacy effective-chain fingerprint.
Both ordinary and probe leases persist the failure; it is never a
`non_recovery_response` and cannot resolve an episode or replenish its budget.
Local backoff uses the hard-quota scale (15 minutes initially, capped at six
hours, with the existing deterministic jitter), without borrowing quota semantics.
An absolute server deadline remains a lower bound. Expiry permits a fenced,
budgeted probe, not an unconditional transition to healthy.

HTTP 401/403 and persistent contract rejections (HTTP 400 and other
client errors) are likewise typed deterministic failures
(`auth_failure`, `contract_failure`) and remain fail-fast. An auth failure
isolates the credential itself: its cooldown scope is the existing quota
identity when one exists, and otherwise the route-throttle scope; models
sharing the dead credential share that cooldown, while a different credential
or endpoint stays available. A contract failure isolates one candidate through
a supplemental candidate scope, hashed like the payment scope and equally
outside the legacy effective-chain fingerprint. Neither outcome is a
`non_recovery_response`: both keep their episode unresolved, record their
failed scope, and never replenish a budget or reset a cursor. Local backoff
uses the same hard-quota scale as payment (15 minutes initially, capped at six
hours). Operator policy probes treat these cooldowns exactly like payment
cooldowns: the deadline is authoritative and a probe is a fenced, budgeted
exception, never a bypass. A chain whose every candidate holds a deterministic
cooldown parks at the earliest cooldown deadline instead of creating empty
model turns, and lineage fallback accounting includes these failure kinds so
an exhausted chain terminates explicitly instead of deferring forever.

One adapter-internal exception exists: a continuation-form dispatch that
carries a prepared lossless replay declares itself before dispatch, and its
contract rejection is settled as a non-candidate-fatal outcome. The adapter's
re-send is the same inference on the same candidate, so the rejection must
not cool down or consume it ahead of that replay. Auth failures never take
this exception: replaying without a continuation reference cannot fix a dead
credential, so the futile re-send stays fenced.

An unavailable scope moves from cooling to a leased probe, not directly to
healthy when its deadline passes. One database transaction reserves every
applicable scope or none; each reservation binds its revision and probe token.
The final dispatch guard must recheck these fences and atomically charge the
durable recovery budget. Local preflight failure releases undispatched leases
without recording a provider failure. Restart cannot renew a lease or budget.
Only a complete response with current fences clears the scopes it proves;
late success/failure cannot overwrite a newer incident or success. A healthy
success is not an incident transition and does not invalidate another healthy
reservation. A concurrent failure still opens a new fenced incident.

Quota, route-throttle and transport deadlines compose by taking the maximum for a candidate.
The chain recheck uses the minimum candidate deadline, subject to the episode
floor. A cooling primary must not prevent selection of a healthy fallback.
Absolute server `Retry-After` deadlines also apply to 5xx transport failures,
independently of quota throttle metadata; restart and operator input cannot
shorten them.
When no candidate qualifies, admission parks before creating a model turn;
durable non-model settlement and inbound storage continue normally.

Automatic recovery includes the initial request in a persistent three-dispatch
budget, with at most one dispatch per failed network, rate-limit, or payment
candidate scope.
`rate_limited` records the admitted quota or route-throttle scope and never
resolves an episode, including when it is the first failure. Subsequent protocol
or other failures cannot replenish that episode. Operator grants deduplicate
route-throttle scopes just as they deduplicate quota scopes, without blocking a
healthy fallback sharing the same connection. Schema migration preserves all
historical request IDs, outcomes, scopes, epochs, revisions and settlement times.
Exhaustion with untried candidates is distinct from chain cooling. Expiry
allows one bounded probe epoch, not a fresh sweep on every wake. Runtime-authenticated
new operator messages can grant one target-bound, idempotent bounded episode;
they do not clear shared incidents or override a server not-before.
Fallback after a provider failure still creates a new turn and preserves all
existing source, owner, lifecycle, and side-effect gates. A typed candidate
cooldown before IO is different: the runtime may replan that unsent inference
within the same turn, without replaying input or completed tools. Each candidate
is checked at most once in that local replan, using the same recovery root and
epoch. Each replacement obtains its own scope reservation and final dispatch
charge; it never borrows the rejected candidate's guard.

This local replan rebuilds the provider, tool surface, native search selection,
and context policy for the selected route. Continuation projection uses the
complete canonical tool-call/result history and the replacement's budget.
Opaque reasoning, provider-owned tool state, or custom tool encodings cannot
cross this boundary: if the completed rounds cannot be represented portably,
the runtime stops explicitly rather than dropping blocks or replaying tools.
Provider window IDs and remote compaction state are never transferred.

Real HTTP attempts before a local cooldown rejection remain part of the same
logical provider round after reselection. Their timeline and token usage are
retained in order on final success or failure, without recording the local
rejection as HTTP. The original requested route and final active/winning route
remain distinct; typed cancellation still terminates the run rather than
becoming a provider failure.

The durable budget binds the validated root execution's source, owner, admitted
fences, and effective-chain fingerprint. Request IDs are consumed atomically
with the final admission-fence check; a duplicate is a denial, not permission
to replay an unknown dispatch. Local construction and rejected reservations
consume no request. A non-network error does not replenish an already active
recovery sequence. Complete usable responses end that sequence; other failures
outside a recovery sequence keep their existing policies.

Ordinary reads and replayed ingress do not start another budget epoch or extend
its floor. A due explicit recheck uses the episode revision CAS, preserving the
cursor, request history, and side-effect boundary. An operator grant cannot be
renewed by reopening its root message or advancing an automatic probe epoch.
Its policy override can skip local cooldown only, never a server not-before or
an existing probe lease. Budget settlement does not itself clear availability
incidents or authorize execution; canonical source and owner checks still apply.

An automatic episode's candidate cursor also preserves an unfinished probe
frontier across epochs and restart. If a later, untried candidate is unavailable,
selection must not wrap to a failed prefix merely because its probe deadline
elapsed: repeated prefix failures can otherwise renew the episode floor and
starve that later candidate under shared cooldowns. A prefix confirmed healthy
by another request remains usable. Once the forward candidates have been tried,
the next sweep may wrap normally. This changes no server deadline, probe lease,
dispatch budget, or canonical execution fence.

An epoch with no dispatched requests is an availability pause, not a failed
epoch: it neither increments the episode's failure count nor restarts its
failure floor. Its next recheck follows the earliest future candidate deadline,
including shared cooldowns and probe leases. If selection supplies no future
deadline, a fixed first-step local delay prevents busy rechecks without
escalating failure history. Epochs with actual dispatched requests retain their
failure floor; repeated parking within an epoch does not restart that floor.

Runtime model requests carry an explicit dispatch guard through transport
lowering. Local prompt, payload, header, and HTTP-request construction complete
before the guard charges IO; SDK retries and redirects cannot silently bypass
it. Canonical execution fences, admission fences, and budget charging share one
writer transaction. An unknown HTTP outcome remains charged.
The guard's rejection is a typed local execution/admission outcome, not a
provider failure. It bypasses provider retry, fallback classification and
cross-turn recovery even when an earlier real HTTP failure remains in the
diagnostic chain. The rejected send consumes no request or candidate cursor;
earlier requests and unknown outcomes are never refunded or erased.
Dispatch diagnostics distinguish canonical source, owner, generation, authority
and recovery-root failures from availability/budget refusals.

Only typed `ScopeCooling` permits the runtime-level local replan above.
Execution, owner, cancellation, storage, unknown admission, episode and budget
refusals remain fail closed. Candidate selection distinguishes normal
unavailability from replan errors: only a normal no-healthy-candidate result may
enter the same-turn wait below. Replan validation, storage or audit failures
terminate with their actual source chain, even if the active admission was
already replaced; they must not fall back to waiting on the old admission.
The latest typed dispatch denial wins over older diagnostic causes, including
anyhow typed context; a foreign error's source is considered only when no such
typed context exists. An older cooldown never overrides a newer safety refusal.
A candidate cooldown alone does not park the
entire source; exhausted chain selection persists one absolute parking deadline,
subject to the recovery episode floor. Diagnostics distinguish an unsent local
candidate rejection, an actual HTTP attempt, and final chain parking.

That replan is bounded to a turn that already completed at least one round. A
candidate cooldown before a turn's first round keeps the existing parking
contract and resumes the same source at the original deadline, so restart and
interrupted-replay recovery stay unchanged.

When a continuation cannot select an independent healthy candidate (including
provider-native history), typed `ScopeCooling` may instead wait within the
current execution. This requires a confirmed unsent request, no in-flight or
unknown outcome, and unchanged canonical authority. Release only undispatched
claims; retain the immutable round request, native blocks, tool results, model,
attempt and recovery root. Never replay the source or rerun a tool. A deadline
is the earliest recheck, not a promise of IO: re-read shared availability and
pass all original execution, source, owner, cancellation, episode and budget
guards before sending. Waiting cannot refund prior requests, charge new ones,
advance candidate cursors, renew epochs or extend an episode's failure floor.
Extended cooldowns use their authoritative absolute deadline; expired or
repeated local denials cannot create a busy loop. Explicit stop cancels the wait.
Operator interjections use an independent input broadcast and return to the
original round's budgeted input boundary without replaying its tools. A final
dispatch input-version fence rejects continuations prepared before newer
operator input, including input arriving during OAuth refresh or compaction.
The fence does not charge a request or bypass authority and budget checks.
There is no new arbitrary cooldown timeout.

The cooling wrapper admits interjections only after the first round. At
round one it leaves queued operator input for the next eligible input boundary
or a later turn; it does not create a second execution or adopt a parked source.

This wait is an active execution substate, not source parking, completion or
a scheduler wake. Expose its current turn/attempt/model, start time, earliest
recheck and safe shared-cooldown cause through a queryable snapshot and
state-change events. Emit entry, deadline/cause changes and exit, not per-second
heartbeats or model-generated briefs. Refresh/reconnect must recover current
state; terminal execution and restart must not leave stale waiting projections.
Distinguish resumption, cancellation and fence rejection. The cause describes
an earlier shared failure, not a new HTTP failure by this turn; publish only
captured classification/status/time/Retry-After and local incident correlation,
never raw errors, requests, response bodies or diagnostic URLs. Missing
historical details must remain explicitly unavailable.

`Superseded` is a reserved cooldown-wait end outcome, not a currently emitted
event. Terminal state cleanup must not be interpreted as such an event; the
current queryable execution snapshot remains authoritative.

Turn-local waiting retains execution memory and ownership. It adds no durable
exact-round checkpoint or cross-daemon-restart continuation permission: existing
recovery and side-effect replay fences remain unchanged. Releasing execution
while preserving an exact native round across restart is a separate contract.

Docker acceptance separates two interjection contracts: `T3772I` interrupts
an actual turn-local cooldown wait while preserving native tool context and
single delivery; `R3773I` races fresh input against an independently parked
recovery source while preserving its owner, absolute deadline, lineage and
single consumption. The legacy `I3754` post-tool-parking oracle remains a
distinct historical case, not a synonym for either replacement. Neither new
case may manufacture parking by reverting the active-wait contract or changing
canonical availability, ownership or recovery fingerprints.

`R3773I` parks recovery `R` before scheduler claim, so `R` remains `Queued`,
not `Interrupted`. Its fresh root `F0` is authenticated operator input with
no causation parent. Both `F0` and `F1` explicitly bind an independent real
WorkItem `W`, created through the authenticated control API while the source
HTTP request is held, then set to `needs_input` to prevent autonomous dispatch.
The fresh turn belongs to `W`; the original source and recovery keep their
original owner. This preserves the existing rule that a later successful turn
with the same owner supersedes older queued recovery, rather than weakening
that rule to satisfy the fixture.
Only after the stub holds `F0`'s real HTTP response may
`F1` interject into that fresh turn. Fresh execution consumes exactly
`{F0, F1}`; recovery consumes exactly `{R, S}`, where `S` is its original
source. Neither interjection provenance nor authority may be upgraded.
The runner derives a usable fresh-provider window from actual incidents
and the original parking deadline `D`, failing closed if there is too
little time. Fresh completion must precede `D`, recovery dispatch must not
precede `D`, and the parked owner, fingerprint and deadline must not change.
This does not cover the separate admission-after-grant `Interrupted` race.

An operator interjection never adopts a source that canonical state marks
`Interrupted`. Such a source stays in the live queue for its own recovery
replay; the interjection drain skips it and still admits later queued inputs, so
a parked source cannot starve fresh operator input. Any other divergent or
unknown canonical status remains a fail-closed conflict rather than an
idempotent reconciliation.

An unrecoverable guard/canonical contradiction while the attempt still owns
its lane settles a WorkItem to `NeedsRepair`, not `Interrupted`/`Runnable`.
Lifecycle and conversation attempts pause. Availability refusals can retain the
original source for its existing provider deadline, then replay the interrupted
canonical lineage without replacing its root or budget. A dispatch rejection
is only a pending disposition, not confirmation that recovery has committed.
For a replay-safe pause, the existing queue terminal transaction validates the
original parking and open attempt, commits interruption, retains the source and
pending projection, and publishes `provider_dispatch_paused` with a nonterminal
acknowledgement. It does not create a failure Brief, `runtime_error`, or current
runtime-failure summary. The audit event retains the dispatch descriptor,
source/attempt identity and absolute deadline; request-level `retryable` remains
distinct from runtime resume responsibility.
The parking deadline still gates admission; restart is not required. Live queue
restoration is a post-commit projection, also reconciled when success is
established by readback. It must not overwrite a newer control-plane Stop.
The synchronous pause commit and its live state/source queue projection share
one agent lock, so concurrent enqueue cannot observe either a stale pre-commit
baseline or the retained pending count without the retained source. Release the
lock before cache updates, event publication, and scheduler notification.
Each settlement OCC attempt derives the acknowledgement and both
`provider_dispatch_paused` / `queue_entry_settled` dispositions from the same
agent-state baseline. A committed Stop selects `suspended_by_control`: the input
and admission deadline remain retained, but explicit Start is required. Otherwise
the disposition is `waiting`, describing retry eligibility rather than
unconditional execution; source, owner, wait and budget fences still apply.
Readback verifies the acknowledgement chosen by the final attempt, not an
earlier waiting proposal. A subsequent Stop does not rewrite historical evidence.
Missing or changed parking, failed settlement, exhausted budget, unknown IO or
a crossed tool side-effect boundary cannot promise safe automatic source replay.
These remain failures or repair blockers; no acknowledgement of automatic
recovery may escape an uncommitted transaction.
An aborted fallback settlement or ordinary interruption does not gain this
automatic reentry. Infrastructure
errors keep their storage classification and retryability while still bypassing
provider fallback. Settlement retains the source and unresolved
task-result responsibility; it cannot replace a newer attempt, clear a business
blocker, or claim that zero dispatch fulfilled a task result.
An exact-wait local stop retains either its direct runtime-owned TaskResult
grant or a synthetic recovery grant. The direct source must be the original
result; a synthetic source requires its internal-followup marker, exact result
identity and charged recovery root. Both require the resolved wait and canonical
owner/turn witness; neither path consumes the result.
Adapters settle each complete HTTP response before auth refresh, lossless
replay, or remote compaction can dispatch another request. A known ordinary
protocol/auxiliary result may end a healthy sequence, but never replenishes
an active recovery episode. Scarce probes skip optional compaction until an
actual usable model response arrives; an auxiliary failure cannot retroactively
discard or masquerade as that model response.

Provider queue parking is separate from business waits. The scheduler checks
canonical eligibility before consulting provider admission, retains the queued
source when parked, and can continue reducer-only bookkeeping behind it. Its
deadline participates in the existing run-loop recheck, without consuming a task
result or moving its owner. A cancelled pre-dispatch build releases its probe;
an in-flight unknown result retains its lease until controlled expiry.
When an exact wait resolution advances the WorkItem source revision, the
scheduler finalizes that transition before creating the provider guard and
commits the same aligned admission plan. Dispatch still checks the canonical
fences exactly. A changed wait or owner snapshot requires replanning the whole
admission and releasing its unused reservation, rather than relaxing fences.
An unrelated AgentState conflict may refresh that baseline with the same guard.

Local context planning failures are not provider attempt failures. A recovery
whose pinned minimum cannot fit terminates through the canonical aborted
queue/turn settlement without dispatching another request or enqueueing another
fallback. Its typed validation diagnostic preserves the effective budget and
required minimum; it does not create or clear shared provider incidents. See
[over-budget failure](./context-candidate-budget-planner.md#8-over-budget-failure).

### 5.6 Recovery Selection Is Message-Bound

The fallback route is carried by a typed directive on the runtime-owned recovery
message. It is accepted only when all provenance fields identify a runtime
instruction emitted by the `model_lineage_recovery` subsystem. Operator,
external, task, or ordinary internal metadata cannot select a fallback model.

The claimed message is resolved once into a turn-local selection snapshot before
the provider, context policy, tool surface, and prompt are built. Those surfaces
all use the same snapshot. `AgentState.pending_fallback_model` remains readable
for state-schema compatibility, but it is not a model-routing source and new
execution paths do not write it.

For configured HTTP routes, canonical provider admission interprets the validated
directive as a preferred candidate, not permission to bypass availability,
capability, lease, budget, or owner fences. The target must belong to the
effective chain, including an agent override and the configured fallback policy.
Recovery never changes the persisted override or implicitly adds the default
model to an override's chain. An unavailable target may be replaced by another
eligible candidate; exhaustion parks before creating a model turn.

The admitted route remains the execution authority and is frozen into the same
turn-local snapshot used by context, tools, lineage, and the actual HTTP request.
`recovery_turn_started` records the requested route separately from its effective
`fallback_model_ref` and `admitted_model_ref`, with a bounded `reselection_reason`
only when the requested and admitted routes differ. Queueing a directive does
not promise that its target will still be admissible at dispatch time.

### 5.7 Recovery Task And Owner Are Source-Bound

The directive identifies the direct failed source Turn, not whichever Turn was
most recent in an owner-filtered history window. Resolve it against durable
same-agent message, Turn terminal, and predecessor execution records before
admission. The recovery attempt inherits the predecessor's complete binding,
including Conversation identity or explicit AgentLifecycle ownership. An
absent source WorkItem must not fall back to the current focus.

If a resolved exact-wait continuation ends in a provider failure before fulfilling
its source, terminal settlement retains that wait's canonical `Waiting` reference
when no newer wait or source revision supersedes it. Recovery must prove the same
durable resolved wait, trigger, predecessor lineage and generation. An independent
business blocker remains intact; it cannot consume the failed continuation or
authorize ordinary blocked, paused or repair-needed work.

An unrelated fresh input must not batch or consume a recovery-paused result,
even under the same owner. Both candidate selection and transactional snapshot
admission preserve that boundary. Explicit admission of the result's own source
or explicit rearm remains distinct; replay of its already charged interrupted
root does not create another recovery charge.

A scanner-queued recovery message is immutable and may predate the exact wait's
registration. Its durable join is the same-agent, same-owner task wait naming
that message as its trigger, whether the wait is `Triggered` or `Resolved`.
An explicit `wait_id` must agree; a conflicting ref cannot use the trigger join
as a fallback. Initial admission, failed-continuation recovery, daemon startup
classification and interrupted claim replay use this same identity. Startup
validates against its own canonical claim in the same transaction; another
activation's live claim remains in flight. Replay must still reject cancelled,
expired or superseded waits, stopped agents, closed owners and stale or consumed
results; resolving a wait does not remove its expiry fence.
The same join and replay lineage apply to an agent-lifecycle exact wait without
a WorkItem; absence of a WorkItem does not revoke the explicit promise or give
an ordinary unbound followup re-entry authority.
Repeated daemon startup validates these runtime-owned successors through the
exact source-to-wait join and canonical lineage, not an agent-message delivery
that their source never created. Ordinary lifecycle delivery fences still apply.

For repeated fallback, follow validated predecessor links to the original task
input with a bounded, cycle-checked traversal. Intermediate recovery messages
and unrelated later operator messages cannot replace that input. Preserve its
original origin and trust: lifecycle or external input does not become operator
input merely because the runtime is recovering it.

The original task body and source relation are pinned context truth independent
of the recent-history window, WorkItemScoped reprojection, and compact anchors.
Hydrate the direct source Turn through the existing history path; its tool
evidence remains subject to ordinary context budgeting. This does not merge all
other-owner history or create a second recovery-history store.

Missing or conflicting source facts, terminal/binding mismatches, and invalid
lineage fail closed with a visible reason and source references. If the pinned
minimum cannot fit, use the existing explicit context-budget failure path;
never call a provider with a silently truncated or missing recovery task.
Diagnostic records carry identifiers and reasons, not additional task bodies.

Recovery audit lifecycle is explicit:

- `recovery_enqueued` means the recovery message and queue entry are durable.
- `recovery_turn_started` is emitted only after that exact message is claimed
  and its turn/run binding exists.
- `recovery_superseded` means a still-queued recovery was atomically dropped
  because a later successful turn for the same owner produced accepted
  continuation progress.

An unrelated, failed, empty, or no-op turn does not supersede recovery. A
recovery that is already dequeued is never cancelled by this reconciliation.

## 6. Provider State Rules

### 6.1 Continuation State Is Lineage-Scoped

Provider continuation state is private to a model lineage.

For OpenAI Responses and OpenAI Codex Responses, this includes:

- `previous_response_id`
- provider-window replay items
- remote compaction items
- encrypted `compaction` / `compaction_summary` content
- unsupported compact endpoint negative cache

Lineage lookup and wire compatibility are separate checks:

1. `ContinuationScopeId` finds the candidate provider window.
2. The provider request shape decides whether that window can be reused.

For the current runtime, the scope is stable for one runtime agent. A missing
scope disables continuation state reads and writes. Changing the OpenAI prompt
cache key does not create a new lineage; it changes the request shape, so the
current request falls back to a full request and installs the new compatible
shape for later rounds.

Changing provider id, model id, transport contract, endpoint kind, request
controls, tool schema, or prompt frame should invalidate the provider
continuation state for the new lineage.

### 6.2 Encrypted Remote Compaction Is Not Portable

Encrypted provider content must not cross lineage boundaries.

Holon may store hashes, byte lengths, item counts, and request-shape metadata
for diagnostics. It must not treat encrypted content as semantic memory or pass
it to a different lineage as ordinary context.

### 6.3 Request Shape Fallback Is A Safety Net, Not The Contract

OpenAI provider-window replay currently compares request shape and falls back
to a full request when the shape changes. This is useful, but it should not be
the only model-switching guard.

The runtime should make lineage reset explicit so diagnostics can say:

- `lineage_changed`
- `pending_model_promoted`
- `lineage_retry_exhausted`
- `deferred_to_fallback`
- `recovery_enqueued`
- `recovery_turn_started`
- `recovery_superseded`

instead of only reporting a low-level request-shape mismatch.

## 7. Tool Protocol Rules

### 7.1 Tool Surface Is Stable Inside A Turn

The tool schema exposed to the model should remain stable for the whole turn.
If fallback promotes a different model lineage, the next turn rebuilds the tool
surface for that lineage instead of mutating the current turn's surface.

### 7.2 No Unpaired Tool Calls Across Turn Boundaries

A recovery turn must not inherit a half-open provider protocol state.

Before terminating the failed turn, Holon should ensure durable records are
closed in one of these forms:

- tool call plus tool result
- tool call plus explicit cancellation/failure record
- assistant text without tool call
- provider failure before any accepted assistant/tool state

The recovery turn should see durable Holon facts, not a protocol fragment that
only the failed provider lineage could interpret.

### 7.3 Side Effects Are Resumed From Evidence

If tools already changed files, spawned processes, or touched external systems,
the recovery turn must continue from current evidence:

- workspace file state
- durable tool records
- work item and todo state
- transcript entries
- audit events

It should not repeat tool work just because the failed provider's hidden
continuation state is unavailable.

### 7.4 Recovery Admission Is Independent From Ordinary Work Scheduling

A cross-lineage recovery message is a typed runtime recovery source, not an
ordinary self-enqueued `InternalFollowup`. When it is bound to an open
WorkItem, canonical admission may claim the WorkItem at its exact current
execution generation while the WorkItem is `Runnable` or `Waiting`.

If the WorkItem is waiting, recovery admission must not clear or resolve that
wait. If the wait is triggered while recovery owns the lane, recovery
settlement restores the exact `Waiting(wait_id)` execution state before the
queued wake is admitted. The recovery directive must bind to a durable
same-agent source message, matching provider-failure terminal Turn, and
terminal execution attempt; the recovery attempt records that predecessor.
`Paused`, `NeedsRepair`, `Terminal`, and already `InFlight` WorkItems remain
ineligible. Conversation and AgentLifecycle recovery are also typed recovery
admissions: they inherit their validated predecessor binding and carry no
WorkItem revision or generation fences. Missing, malformed, or source-conflicting
directives fail closed rather than falling back to ordinary internal-follow-up
scheduling. These rules do not broaden ordinary timer, task, or external wake
authority.

## 8. Runtime State Model

Suggested runtime additions:

```rust
struct TurnLineageState {
    requested_model_ref: ModelRef,
    pending_model_ref: Option<ModelRef>,
    pending_fallback_model_ref: Option<ModelRef>,
    active_model_ref: Option<ModelRef>,
    active_lineage_key: Option<ModelLineageKey>,
    side_effect_boundary_crossed: bool,
    recovery_attempt: u32,
}

struct ModelLineageKey {
    provider_id: ProviderId,
    model: String,
    transport: ProviderTransportKind,
    endpoint_kind: String,
    tool_surface_hash: String,
    request_controls_hash: String,
}
```

The exact shape can be smaller if the provider request shape already carries
some fields. The important property is that lineage-sensitive state has a
first-class identity and reset reason.

## 9. Implementation Plan

1. Add diagnostics-only lineage tracking to provider attempt timelines.
2. Add `turn_active_model_ref` to the turn loop and keep one active lineage for
   the whole turn.
3. Teach provider retry to stay within the active lineage for the current turn.
4. Replace in-turn cross-lineage fallback with terminal records plus queued
   next-turn fallback/recovery.
5. Add terminal kinds for retry exhaustion that distinguish
   `deferred_to_fallback` from `provider_failed_needs_recovery`.
6. Add bounded recovery/fallback-turn enqueueing with loop prevention and bind
   the fallback route to the recovery message identity.
7. Reset provider continuation explicitly when pending model or fallback
   selection is promoted at the next turn.
8. Reconcile legacy `pending_fallback_model` state at bootstrap by discarding
   the global slot; a durable typed recovery message, when present, remains the
   only authoritative fallback selection.
9. Add tests for:
   - retry stays on the same lineage inside a turn
   - pre-output cross-lineage fallback terminates the turn and queues a new one
   - post-output cross-lineage fallback terminates as recovery
   - recovery turn starts as a new turn
   - ordinary queued messages cannot consume a recovery fallback
   - enqueue, actual start, and supersession are distinct audit transitions
   - encrypted remote compaction is not replayed across lineage change
   - user model switch does not affect an already-running turn

## 10. Open Questions

- Should automatic recovery be enabled by default or gated by runtime config?
- What terminal kind name should be used for locked-lineage provider failure?
- Should recovery messages be operator-visible by default?
- Should a recovery turn count against the same run budget as the failed turn?
- How should Holon surface repeated fallback-loop prevention to the operator?

## 11. Decision

Holon should converge on this rule:

> One agent turn uses one selected model lineage. Retries stay within that
> lineage. Cross-lineage fallback ends the current turn and continues, if
> enabled, as a new turn from durable Holon state.

This keeps provider fallback useful without letting hidden provider state,
encrypted remote compaction, or model-specific tool contracts leak across
semantic turn boundaries.
