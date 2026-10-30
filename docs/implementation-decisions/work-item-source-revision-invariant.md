# WorkItem source revision invariant and synchronization points

Issue: #3816

## Choice

An open WorkItem's execution-protocol `source_revision` must equal its
durable `work_items.revision` in the same transaction that advances either
side. This invariant is enforced at two layers instead of at each writer:

1. The queue transaction core synchronizes every WorkItem mutation it
   applies, no matter which payload channel (wait registration, wait
   transition, or completion) carried it. Previously only the wait
   registration channel synchronized, so claims resolving a wait and
   completion co-updates could strand the registration behind durable
   revisions.
2. `validate_execution_commands_tx` ends with a final equality assertion:
   every open WorkItem mutation's revision must equal the reduced final
   registration source revision. Any residual writer combination now fails
   closed inside its own transaction instead of persisting drift.

## Reason

The dispatch guard and settlement bail were already correct fail-closed
checks; the observed back-to-back admission rejections and post-model
settlement loops both came from writers that advanced the durable revision
without the registration. Enforcing the invariant once, at the commit core,
keeps every writer honest without adding a new lifecycle or relaxing the
"reject unknown drift" boundary from #3791. Existing drift is never adopted
implicitly: settlement keeps rejecting it, and the pre-IO dispatch guard
blocks further model turns for the agent with a
`provider_dispatch_work_item_drift_rejected` audit event so a stranded
registration stops burning tokens.

## Preserved boundary

Historical drift is repaired only by an explicit, offline, evidence-fenced
operation (`holon debug runtime-db work-item-source-revision`), which adopts
a registration back to the durable revision only when the WorkItem is open,
not executing, and every intermediate revision has a `work_item_written`
attribution event. The normal `AdvanceWorkItemSourceRevision` chain still
requires its same-transaction durable mutation and keeps rejecting
`already_durable`; the repair path never rewrites admitted fences,
generations, wait state, or attempt history.
