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

Model-reentry admission now derives one `canonical_activation_plan` for
WorkItem autonomous continuation, exact task rejoin, exact wait resume, and
explicitly bound operator input. The plan carries the typed cause, binding,
provenance, expected WorkItem scheduling generation, expected agent dispatch
revision, exact wait/task identity when applicable, and the legacy queue/Turn
compatibility binding. Its optional demand registration and wait trigger,
authority issuance, activation admission, legacy queue claim, and running
projection commit in the same `QueueTransitionCommand`.

Operator interjection is deliberately not a second admission. At a safe point
the runtime commits a typed `ActivationInputAttachment`, the legacy
`Queued -> Interjected` transition, transcript evidence, and audit evidence in
one transaction. The attachment is fenced by the running activation, WorkItem
scheduling generation, dispatch revision, message, Turn, boundary, and round;
it does not reserve another slot or advance WorkItem scheduling state.

The semantic decision plane returns `Ok(None)` when trusted ingress
conditions are not met, rather than propagating the error. This prevents
observation and audit mechanisms from blocking the run loop or causing test
deadlock.

The `Authoritative` rollout mode is scenario-local and fail-closed. A queue
transition is accepted only when the same transaction carries matched
canonical comparison evidence for the authoritative scenario. Missing or
divergent evidence rejects the queue, projection, audit, comparison, and
protocol writes, while the same transaction records a revision-fenced hard
blocker and atomically returns that scenario to its configured `Shadow` or
`Off` rollback target.

`QueueTransitionCommand` declares its authority scenario requirements
separately from the optional comparison payloads and carries the exact rollout
configuration, manifest, preflight, and production-capability expectation read
before the boundary. This lets the repository reject stale authority or an
omitted payload before any queue mutation instead of treating absence as an
unscoped legacy transition.

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
validator retain all state-transition control. In `Authoritative` mode, legacy
observation remains compatibility evidence, but Runtime-owned validation and
transaction commit decide whether the transition is admitted. Semantic
proposals cannot satisfy, bypass, or weaken the matched-evidence requirement.

The generalized plan only covers structurally exact scenario classes. Ordinary
unbound operator input and ambiguous reentry remain outside canonical
admission. Legacy queue, AgentState, and Turn facts remain compatibility
participants in the transaction rather than independent admission authority.
This decision does not choose which scenario classes are promoted to
`Authoritative`; rollout policy and promotion gates remain separate.
