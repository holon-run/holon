# Turn-end commit no longer writes the WorkItem blocker

Issue: #4467

## Choice

`maybe_commit_turn_end_work_item_transition` writes a new `WorkItemRecord`
snapshot only when the plan artifact changed. It never derives, overwrites, or
clears `blocked_by` / `blocked_by_wait_id`; both fields are carried over
verbatim from the bound snapshot.

The turn-end blocker helpers (`turn_end_wait_blocker_state`,
`waiting_reason_blocker`) and the unreachable `BlockedRecheck` queries
(`storage::due_blocked_work_item_rechecks`,
`storage::next_blocked_work_item_recheck_at`, and their
`WorkItemRepository` counterparts) are removed.

## Reason

`WorkItemRecord.blocked_by` is descriptive display text. The read model
already treats it as non-authoritative: it does not gate scheduling, does not
wake the runtime, and clearing it does not create a scheduling generation
(`docs/rfcs/work-item-scheduling-read-model.md`,
`docs/rfcs/scheduler-wait-state.md`).

The remaining turn-end write path contradicted that contract in two ways. A
turn that ended on a waiting reason replaced the annotation with canned text
(`"Waiting on operator input."`, `"Waiting on a timer."`, …) that no agent
authored, and a turn that ended without a waiting reason silently cleared an
operator-authored annotation written through the explicit update path. The
result was a display field whose content depended on turn boundaries rather
than on the writes that own it.

## Preserved boundary

The explicit write paths stay the only mutators of the blocker:
`PickWorkItem(clear_blocker = true)` still clears it together with the
ownership fields, and `UpdateWorkItem` still sets it.

Two follow-ups stay out of scope and remain unchanged:

- The display layer still derives `WorkItemFocus::Blocked`,
  `TriggeredBlocked`, and queue ordering from `blocked_by.is_some()`, so a
  legacy canned annotation keeps its item displayed as blocked. Decoupling
  that display derivation is a separate decision.
- Existing rows keep their stored annotations. There is no bulk cleanup
  migration, and the historical work-item recheck columns stay readable.

