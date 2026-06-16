# Event and timeline projection audit

This note records the current state of runtime events as consumed by the local
TUI and the web GUI timeline. It is intentionally an implementation audit, not a
new event contract.

Related source:

- `src/tui/projection.rs`
- `src/presentation.rs`
- `web-gui/app/src/runtime/session-reducer.ts`
- `web-gui/app/src/runtime/runtime-store.ts`
- `docs/rfcs/event-stream-interface.md`

## Current assembly model

The first-party clients consume the same event stream but assemble display state
independently.

- The TUI keeps a bounded `event_log`, a smaller durable conversation log, and a
  message cache. It applies state-sync events directly for agent/tasks/timers/
  work items, and sends non-state-sync events through the presentation reducer.
- The TUI presentation reducer turns raw events into operator-facing items,
  deduplicating repeated operator messages, repeated briefs, assistant previews
  that match final briefs, and command start/finish style updates.
- The web GUI keeps event pages and live stream deltas in `eventsBySeq`, then
  rebuilds a per-agent timeline through `reduceAgentSessionTimeline`.
- The web GUI also keeps a per-agent message cache and hydrates slim
  `message_enqueued` events through `messages:batchGet`.

The direction is sound: audit events should be lightweight lifecycle facts, and
large display content should come from message, brief, task-output, or
presentation storage.

## Event use by family

| Event family | Current TUI use | Current web GUI use | Assessment |
| --- | --- | --- | --- |
| `message_enqueued` | Conversation item for operator-origin messages. Slim payloads are hydrated from message storage; old full-message payloads still work. | Timeline operator item when origin is operator. Slim payloads are hydrated through `messages:batchGet`; otherwise the item shows a loading placeholder. | Keep as lifecycle event. Do not carry full text long-term. The event needs stable `message_id`, origin summary, and timestamp/provenance only. |
| `message_admitted` | Not a primary presentation event. | Falls through to debug runtime event if present. | Mostly audit/debug. It overlaps with `message_enqueued` for UI purposes and should not be promoted into timeline unless it represents a distinct admission failure or trust decision. |
| `message_processing_started` | Used as an activity boundary/reset signal and debug/status evidence. | Debug event only. | Keep slim. It is useful for run correlation but redundant as a visible timeline item. |
| `turn_started` | Projection summary and activity boundary/correlation. | Falls through to debug event. | Debug/correlation only. It overlaps with `message_processing_started` for visible display. |
| `operator_interjection_admitted` | Summarized with `text_preview`. | Falls through to debug unless explicitly projected later. | Keep, but only with preview. It represents a specific interjection state transition rather than ordinary input. |
| `brief_created` | Conversation result item. Supports both historical full `BriefRecord.text` and slim `BriefCreatedAuditEvent.text_preview`. | Info timeline item, roster activity source, and bootstrap `lastBrief` patch source. Current web code still expects `text` in some paths. | Keep as lifecycle/result event, but payload should remain slim. Web UI should use `text_preview` for list/timeline previews and fetch full brief text from a brief API when needed. |
| `assistant_round_recorded` / `text_only_round_observed` | Assistant progress preview. Deduplicated when it matches a final `brief_created`. | Verbose assistant preview. Deduplicated when it matches final brief text. | Useful only as progress/debug preview. It duplicates final brief content by design, so it should stay preview-bounded and lower visibility. |
| `provider_round_completed` | Provider/model progress detail. | Debug event through `provider_` prefix. | Debug/diagnostic only. Avoid carrying large prompt/response content here. |
| `tool_executed` / `tool_execution_failed` | Main tool/command/action projection. Special cases for commands, `ApplyPatch`, work-item mutations, `ListWorkItems`, sleep, and failures. | Verbose tool item. Special cases for `ApplyPatch`, work-item tools, `ListWorkItems`, `GetWorkItem`, `ViewImage`, command output previews, and errors. | Keep, but payload should contain bounded summaries and artifact refs, not full command output or huge tool results. Some tool result fields duplicate external task/output artifacts and should stay preview-only. |
| task lifecycle events (`task_created`, `task_status_updated`, `task_result_received`, child/recovery/command-task failures) | TUI updates task state for canonical task events and presents task lifecycle notices for several task events. | Most `task_` events are debug. Active task display comes primarily from agent state/detail, not timeline. | Keep lifecycle events slim. Full command stdout/stderr belongs in task output APIs/artifacts. Visible timeline should usually show only task id, status, summary, and failure reason. |
| timer/wait/resume events (`timer_created`, `timer_fired`, `wait_condition_registered`, `wait_conditions_*`, `waiting_intent_*`, `callback_delivered`, `continuation_trigger_received`, `continuation_resolved`) | Timers update timer state; fired callback/continuation events can become resume notices; waits can become waiting notices. | `wait_condition_registered` and `agent_waiting` are visible system/waiting items; many others fall through to debug. | Keep. Several events describe different layers of the same wait lifecycle, so only one should be operator-visible by default. Other layers are debug/correlation. |
| work-item lifecycle events (`work_item_written`, `work_item_picked`, `work_item_focus_released`, continuation/delegation/turn-end/stale-reminder events) | `work_item_written` updates work-item state and selected transitions become cards/bookkeeping. Many work-item lifecycle events become bookkeeping. | `work_item_*` events are verbose or debug; work-item mutation tools are hidden from normal tool timeline unless debug projection asks for them. | Keep `work_item_written` as the canonical state-change event. Many surrounding events duplicate scheduling/bookkeeping and should remain debug/verbose only. Use previews/objective snippets, not full plan or completion bodies. |
| agent/session state events (`agent_state_changed`, legacy `session_state_changed`, `closure_decided`, `control_applied`, lifecycle shutdown/posture events) | `agent_state_changed` drives TUI state; `closure_decided` updates closure and may present an internal transition. | `closure_decided` is debug; agent detail/bootstrap drives most visible state. | `agent_state_changed` is state sync, not conversation. `session_state_changed` is legacy replay-only and should not be used for new transitions. |
| workspace/worktree events (`workspace_*`, `worktree_*`, cleanup/metadata events) | Summarized for log/debug; some workspace/worktree enter/exit events have readable summaries. | Fall through to debug unless they match generic error/failure. | Debug/status only. Avoid long paths where an id plus basename/branch is enough; full metadata should be available from workspace/task detail. |
| scheduler/diagnostic/recovery events (`scheduler_decision`, `scheduler_diagnostic`, retry/lineage/context/compaction events) | Mostly log/debug. | Mostly debug fallthrough. | Keep debug-only. These are important for support but too noisy for timeline. |
| storage/test/bootstrap events (`db_canonical_*`, `live_event`, `test_event`, `legacy_chat_event`) | Not intended as user timeline items. | Debug fallthrough if exposed. | Internal/debug only; should normally not appear in first-party operator timeline. |

## Repeated or redundant event layers

The following repetitions are expected in the ledger but should not all become
visible timeline entries:

1. **Input lifecycle duplication**
   - `message_admitted`, `message_enqueued`, `message_processing_started`, and
     `turn_started` can all refer to the same operator input.
   - UI should show at most one operator message, backed by `message_enqueued`
     plus message hydration.
   - The other events should remain debug/correlation unless they carry a
     distinct failure, trust, or queueing decision.

2. **Assistant preview versus final result**
   - `assistant_round_recorded` and `text_only_round_observed` can contain text
     that is later repeated by `brief_created`.
   - Both TUI and web already try to hide assistant previews that match final
     brief text.
   - The preview event should keep only `text_preview` and metadata. It should
     not carry full final-result text.

3. **Tool execution versus command/task artifacts**
   - `tool_executed` often carries command previews, summaries, output previews,
     result metadata, and task ids.
   - Command stdout/stderr and long tool outputs are separately available from
     task output or tool-output artifacts.
   - Timeline events should keep bounded previews and stable refs. Long output
     should not be duplicated in event payloads.

4. **Work item tool calls versus work item lifecycle events**
   - A `CreateWorkItem`/`UpdateWorkItem`/`PickWorkItem`/`CompleteWorkItem` tool
     event and a `work_item_written`/`work_item_picked` lifecycle event can
     describe the same user-visible change.
   - The web GUI already hides successful work-item mutation tools from normal
     projection. The TUI presentation reducer also suppresses successful
     work-item mutation tool events.
   - The lifecycle event should be the canonical UI source; the tool event is
     execution evidence and debug detail.

5. **Wait lifecycle layering**
   - A single wait/resume path can produce wait registration, timer creation,
     callback delivery, continuation trigger, continuation resolution, and
     closure changes.
   - User-facing display should show a concise waiting/resumed item. Lower-level
     callback/timer/continuation events should remain debug unless something
     fails.

6. **Agent state sync versus specific lifecycle events**
   - `agent_state_changed` can repeat facts also visible through closure, task,
     work-item, and current-run events.
   - Clients should treat it as a state-sync snapshot/delta source, not as a
     timeline item.

## Long-field duplication risks

These payload fields are the main duplication risks:

- message bodies in `message_enqueued`;
- brief full text in `brief_created`;
- assistant full response text in `assistant_round_recorded` or
  `text_only_round_observed`;
- command stdout/stderr and huge command summaries in `tool_executed`;
- full tool result JSON in `tool_executed`/`tool_execution_failed`;
- full work-item objectives, plans, completion reports, or todo lists in
  work-item lifecycle events;
- full workspace paths and worktree metadata repeated in every workspace event.

The preferred shape is:

- event payload: ids, lifecycle action, timestamps, origin/trust/provenance,
  short previews, lengths, status, and artifact refs;
- detail API/storage: full message, full brief, full task output, full work-item
  record, full tool output, full workspace metadata.

## Client-specific gaps found during the audit

1. The web GUI has message hydration for slim `message_enqueued`, but brief
   handling is not consistently updated for slim `brief_created`.
   - `session-reducer.ts` still uses `payload.text` and otherwise renders
     `Brief text unavailable.`
   - `runtime-store.ts` and `client.ts` also derive roster/bootstrap last-brief
     text only from `payload.text`.
   - These paths should use `text_preview` for preview display and eventually
     a brief detail API for full text.

2. The web GUI projects many unknown runtime events as debug timeline items.
   This is useful during development, but the event taxonomy is now large enough
   that display policy should classify by event family rather than relying on
   prefix fallthrough.

3. TUI and web implement similar dedupe rules independently:
   - operator message dedupe;
   - assistant preview versus final brief dedupe;
   - work-item mutation tool suppression;
   - command/tool special casing.

   This is acceptable while the event stream remains raw, but the rules should
   stay aligned through tests or a shared documented projection policy.

## Recommendations

1. Keep the raw event stream as the canonical replay surface, but treat it as
   lifecycle/audit data, not a complete display transcript.
2. Make `message_enqueued`, `brief_created`, task lifecycle, and work-item
   lifecycle events permanently slim.
3. Add or stabilize detail APIs for full brief content before requiring UI to
   display full historical result text from slim events.
4. Use one visible event per user-facing semantic action:
   - operator message: `message_enqueued` + message hydration;
   - assistant final result: `brief_created` + brief hydration when needed;
   - tool action: `tool_executed`/`tool_execution_failed` with bounded preview;
   - work-item state change: `work_item_written` / focused lifecycle event;
   - wait/resume: highest-level wait or resume event, not every timer/callback
     layer.
5. Keep duplicated lifecycle layers available in debug mode for recovery and
   support, but avoid rendering them in the default timeline.
6. Prefer `*_preview`, `*_len`, ids, and artifact refs over full text fields in
   audit event payloads.
