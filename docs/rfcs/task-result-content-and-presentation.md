---
title: Task result content, wake authority, and conversation presentation
status: implemented
---

# Task result content, wake authority, and presentation

A command or supervised child task produces result content. An existing-agent
invocation instead observes the first accepted later message from that sender;
its completed handle is a message-received signal, never business completion.

For an interrupted supervised invocation, follow its unique canonical recovery
successor first. Without a successor, an aborted or quarantined queue entry for
that exact attempt's source proves failure; report only its bound turn/failure
brief. A recoverable interruption remains pending, and another source's failure
is not evidence about this invocation.

Successful agent-message waits publish a stable TaskResult containing message
and delivery references, marked by `task_detail.reply_content_source=original_message`.
Older copied-content results retain their existing delivery semantics. Only the original agent message carries peer content
and its original provenance. Terminal task publication atomically binds an exact
task wait to the original response message. Admission reconciles accepted replies
before the original message can outrun its asynchronous monitor. The original
message claims the existing exact-wait authority and resolves the wait in the
same transaction as its canonical activation. Reference signals are reducer-only.
Late WaitFor uses the same original message and the settlement obligation, not
queue processing alone, to distinguish delivered, currently admitted, in-flight,
recoverable and paused results. An explicit waiter in another WorkItem admits the
promised task's settlement with its exact-wait activation; unrelated owners that
observe the same reply retain their separate obligations. Cancellation does not consume the peer delivery.

Task completion, caller admission, and successful activation settlement remain
separate durable facts. The existing settlement ledger retains deferred results,
restart recovery, and generation fences. A reply can satisfy concurrent observer
handles without copying its content. One trigger marker per response remains the
current same-agent wait constraint; this change does not introduce general
multi-owner notification fan-out or strict request/reply matching. Once the original
reply is processed, non-selected observations settle as `reply_consumed_elsewhere`
without claiming delivery to those owners or scheduling further rechecks.

Provider fallback atomically inherits the validated predecessor's unsettled
admission grant and input evidence before its first request. Successful terminal
settlement belongs to the receiving activation; the immutable result, source
message, captured task owner and ingress turn do not change. This includes live
inputs and exact waiters in a different WorkItem. Failed attempts do not fabricate
delivery. See [bounded input consumption](bounded-input-consumption.md).

Runtime task results are presentation data, distinct from assistant responses.
Model-reentry tasks publish their normal model response without a synthetic
assistant brief. Reducer-only results are explicitly task-linked. Conversation
projections expose bounded task status/summary/preview metadata, and Web, Android, and iOS
render task results inside their receipt turn's
expandable execution process, with inspectable source messages. Durable reducer-only turn reasons classify historical runtime-generated
briefs independently of their text or missing task link. No-model results, including
command results, retain a visible outcome.
Task IDs and complete raw output belong in the inspector. Pending inputs retain
their existing background-event placement. Historical evidence is preserved.

## Supervised invocation recovery

A result-bearing new child is bound to its original task-origin input, not the
child's latest turn, brief, observability phase, or agent-wide idle state. Source
message/turn linkage and the parent task's child/incarnation target are committed
atomically with the original child queue admission. Recovery never resends the
delegated prompt. Legacy records may recover linkage only from one matching
task-origin input with the same parent/supervision identity; missing or ambiguous
evidence fails closed. A missing baseline is not replaced with the current turn.

The host reuses the canonical invocation walker for original source admission,
recovery successors, exact wait identities, task-result continuations and
WorkItem handoffs. A yielded caller remains pending until that caller completes;
the intervening WorkItem's completion is not the invocation result. Results use
the exact terminal turn transcript or completion brief. Later unrelated turns,
tasks and waits cannot replace an already proven result. Active blockers without
source ownership cannot be ignored. Missing canonical evidence, conflicting
identity and ambiguous/cyclic continuations are recovery-blocked, not completion.

Terminal task detail retains child observability and worktree metadata on both
live and recovered paths. Observability describes the proven invocation terminal
turn/result, not later child activity. A loaded runtime may supply worktree and
usage state without becoming terminal evidence; unloaded recovery remains
storage-backed and does not load a child merely to enrich metadata.
Worktree preparation persists parent task metadata before source binding; source
admission uses the post-preparation task snapshot and retains exact CAS checks.
Running state is persisted directly rather than queued as a stale task snapshot.

`task.detail.recovery_diagnostic` distinguishes normal `pending` from
`recovery_blocked`, with typed reason/stage, target incarnation and bounded retry
state. Source/terminal references remain in task detail; diagnostics contain no
prompt, report body or arbitrary error chain. Unchanged diagnostics are not
rewritten on every probe. Reattachment is supervision registration, not a child
restart or proof of result publication.
Diagnostic deduplication advances only after a successful write; a failed write
must retry the unchanged reason after storage recovers, without logging every
probe.

Terminal task/result/queue/settlement publication continues through the existing
atomic task transition, with one stable result identity per invocation. Repeated
matching publication is a no-op; conflicting source/terminal evidence is rejected.
The existing three transaction-conflict retries remain unchanged. If publication
still fails, the monitor retains responsibility with capped-backoff rechecks and,
when writable, persists a commit-stage recovery diagnostic. An unavailable store
cannot promise a durable diagnostic: the runtime emits a structured content-free
failure and the active task/source remains restart-recoverable.

`Retain` does not delete a child. Legacy `DeleteOnTerminal` keeps its existing
transactional deletion admission. Result publication does not imply delivery,
admission or consumption: captured owner, exact wait generation, source/consumption
fences and automatic admission budgets remain unchanged. Existing-agent
message-observation handles and command recovery retain their separate semantics.

## Conversation process projection

TaskResult remains a canonical turn input, not a new server activity. Clients
combine task input rows with activity rows only for presentation. Initial task
inputs precede that turn's activities; interjected inputs use their durable
activity key when supplied. Late results stay in the turn that received them.
No cross-turn regrouping or message-body equality heuristic is introduced.

Completed process sections default to collapsed. A runtime-only result keeps a
compact title/status header even when the turn has no assistant brief. Failure
and interruption show the task status and first reason line while collapsed
(command previews prefer the error/output cause or exit status to the repeated title);
that status does not replace the receiving turn's own execution status. User
interjections, manual disclosure state, and reading anchors retain their normal
behavior. Pending results remain in a separate collapsed background-event group.

Expanding a process reveals bounded task preview and timestamp. Full source
reads happen only through explicit detail actions. Reference-only agent replies
have no copied preview and offer a view-original-reply action. Completed reply
observations say "Agent reply received", retaining the distinction from business
completion. Native clients
reuse the authorized task-output reader, which dereferences the original reply
and explicitly labels truncation; Web opens the original message inspector.
Historical runtime briefs are filtered by task/message links and durable
runtime provenance, never by equal text. Explicit `runtime_only=false` preserves
a genuine model response even when related IDs match. Missing optional metadata
remains compatible with older daemons. Runtime settlement and delivery authority
are unchanged.
