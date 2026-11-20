# Charged task-result recovery root replay ordering

## Decision

Queue selection boosts only two message classes over older provider
lineage retries: the `task_result_recovery` root message itself (already
the case) and the `model_lineage_recovery` replay whose provider recovery
directive's `root_message_id` names the unsettled settlement's latest
charged recovery root. The replay's own episode cooldown and queue
parking gates still decide when it may dispatch; the boost decides only
ordering.

## Reason

The scheduler already held that "older provider lineages must not starve
an attached result snapshot for this owner, including its final charged
root", and boosts the root message accordingly. Once that root's turn
fails and defers its fallback, the replay successor previously lost all
priority: it was enqueued behind every older lineage successor, and
those successors consumed the shared transport-scope grants each
cooldown cycle. With the primary route faulted, the final charged root's
lineage could then never reach the fallback route, so task-result
recovery stopped converging and the strict recovery acceptance gate
(`restart-fifth-exhausted`) timed out.

## Alternatives considered

Honoring the deferred-fallback model ref strictly (waiting for the
directed candidate instead of reselecting a freed candidate) was
rejected: payment-required and credential cooldowns legitimately
reselect a healthy candidate for hours, and that behavior is an
intended, tested contract (`requested_candidate_cooling`). Updating the
acceptance oracle instead was rejected because the runtime's own
invariant (the charged root must not starve) is the violated side.

## Preserved boundary

The boost changes no provenance, authority, or timing decision, and
cannot cross operator or interject input; it only reorders runtime-owned
internal follow-ups that share the obligation's owner.
