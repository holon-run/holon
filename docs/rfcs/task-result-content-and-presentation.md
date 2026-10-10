---
title: Task result content, wake authority, and conversation presentation
status: implemented
---

# Task result content, wake authority, and presentation

A command or supervised child task produces result content. An existing-agent
invocation instead observes the first accepted later message from that sender;
its completed handle is a message-received signal, never business completion.

Successful agent-message waits publish a stable TaskResult containing message
and delivery references, marked by `task_detail.reply_content_source=original_message`.
Older copied-content results retain their existing delivery semantics. Only the original agent message carries peer content
and its original provenance. Terminal task publication atomically binds an exact
task wait to the original response message. Admission reconciles accepted replies
before the original message can outrun its asynchronous monitor. The original
message claims the existing exact-wait authority and resolves the wait in the
same transaction as its canonical activation. Reference signals are reducer-only.
Late WaitFor uses the same original message and reports continue-turn when that
message was already consumed. An explicit waiter in another WorkItem admits the
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
