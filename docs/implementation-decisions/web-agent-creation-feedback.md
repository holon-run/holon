# Web Agent Creation Feedback Boundary

The Web create action returns `accepted`, `rejected`, `uncertain`, or `stale`,
instead of a boolean. An accepted receipt establishes the identity and
incarnation used for navigation. A degraded bootstrap is still an accepted
creation, not permission to submit another create.

Roster refresh and bounded bootstrap-job observation run independently after
acceptance. Jobs are volatile; completion, disappearance, failure, and timeout
are reconciled against the durable bootstrap in agent detail. Missing or
unavailable observations remain unconfirmed. Client-generation, receipt, and
incarnation checks prevent old observations from updating a replacement.
Connection, authenticated-subject, runtime, epoch, and visibility-scope changes
invalidate creation records and dialog identity checks. A per-creation
observation revision fences late success and error responses from older
manual or automatic detail reads.
Initialization completion means its steps, including initial-message enqueue,
finished; it does not promise a successful first turn.

Creation dialogs own their errors; they never overwrite template-catalog
diagnostics. Only allowlisted reasons, HTTP status, and bootstrap step names
and statuses are displayed. Raw HTTP messages, job summaries, and bootstrap
`last_error` can contain private paths, SQL, or credentials, so they are not
rendered here. Unknown failures direct the operator to existing-identity,
deletion-status, and runtime diagnostics without inventing a root cause.

An uncertain POST is never automatically resubmitted. A read-only identity
check may offer opening an active existing identity with its authoritative
incarnation; neither a 409 nor a failed lookup is evidence of successful
creation or safe retry. These UI observations are session-local and do not
replace durable runtime state or imply automatic repair, resend, or rerun.

`baseline_over_budget` is an existing terminal execution outcome and is shown
as a run failure with model/budget guidance. This change does not alter model
budget resolution or deletion/recreation admission.
