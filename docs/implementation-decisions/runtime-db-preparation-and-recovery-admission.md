# Keep preparation reads and recovery writes on their respective DB roles

Use a reserved reader pool for canonical preparation and reconstruction
from existing Input/Scheduling qualification. The ordinary reader pool
cannot starve cold runtime construction before its asynchronous bootstrap
handoff. Both pools share the original deadline, cancellation, cleanup and
protection rules; neither may overflow or fall back to the writer.
The [reader reuse RFC](../rfcs/runtime-db-reader-reuse.md#configurable-reader-limits)
defines startup-configurable limits, defaulting to 16 ordinary and 4 preparation
readers; reservation is per role, not a single cross-Agent preparation lock.
Missing qualification alone may
enter the original initialization transaction under the held mutation owner.
Keep first initialization atomic; do not relax canonical drift, membership,
identity or publication checks to make a read succeed.

Qualification-capable receipt recovery joins the ordinary writer FIFO and
reuses its connection. Reserve Immediate before receipt observation, preserve
the complete owner group and attempt, and invoke the recovery callback once.
This prevents a second raw writer from timing out against the runtime's own
writer, without risking a Deferred snapshot upgrade or replaying business work.

Recovery failures retain their own transaction certainty for diagnostics. They
do not prove that the original business attempt rolled back. Capsule cleanup
and CAS refresh use original-attempt certainty, so a failed recovery keeps the
same frozen descriptor until its original receipt/owner protocol resolves it.

Borrowed callbacks stay on the original thread through the existing blocking
boundary because their material and mutation witnesses are thread-local.
Moving all such callbacks to the background writer would require a separate
owned-command contract. Success continues to mean durable commit, not queue
acceptance. Unknown transaction outcomes retain their reconciliation fence.
