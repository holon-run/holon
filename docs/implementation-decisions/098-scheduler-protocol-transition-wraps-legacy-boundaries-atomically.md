# Scheduler Protocol Transition Wraps All Scheduler Boundaries Atomically

## Choice

Every scheduler boundary (message admission, work-queue idle tick, operator
interjection, and turn-end queue transition) commits through a single
`QueueTransitionCommand` that wraps the queue operation, agent state
projection, message evidence, audit events, shadow comparison, and semantic
shadow decision in one SQLite transaction. After Phase 5a–5e, the same
transactional wrapping extends to wait resume (`.or_else` within message
admission), settlement recovery and delivery disposition (within
`commit_queue_settlement`), and operator interjection at four typed
boundaries (`AfterProviderRound`, `BeforeToolExecution`, `AfterToolResults`,
`BeforeProviderContinuation`). A public `SchedulerDiagnosticAuditEvent`
stream is emitted alongside the legacy audit for every decision.

The semantic decision plane returns `Ok(None)` when trusted ingress
conditions are not met, rather than propagating the error. This prevents
observation and audit mechanisms from blocking the run loop or causing test
deadlock.

The `Authoritative` rollout mode is fail-closed: if production authority is
not connected, all admissions are rejected. This is an MVP gate, not a
production cutover. The mode exists to verify that the protocol can enforce
admission control, not to operate the scheduler.

## Reason

Wrapping all boundaries in the same transaction prevents partial shadow
samples from surviving a CAS conflict or transaction failure. If the legacy
claim commits but the shadow comparison does not, the comparison record
contradicts the actual admission state and cannot be trusted for divergence
detection.

Returning `Ok(None)` from the semantic shadow when trusted ingress is absent
separates the observer from the authority: the semantic plane is additive
observability, not a gate. If it propagated errors, any message that did not
match trusted-ingress construction (including test fixtures with empty
`message_seq`) would block the entire run loop.

## Preserved Boundary

The legacy scheduler remains the sole production authority in `Shadow` mode.
The protocol layer records comparison and semantic evidence but does not
reject, redirect, or alter legacy decisions. No provider, model, or semantic
plane component owns runtime authority; the deterministic resolver and
validator retain all state-transition control. `Authoritative` mode is not a
production path until canonical evidence pass-through is implemented.
