# Scoped Runtime DB Reads

## Status

Stage 3A provides opt-in pooled-reader infrastructure. Stages 3B/3C wire the
list, state, roster-snapshot and projection-snapshot routes to `run_owned`,
scoped blocking reads and an explicit service owner. This integration is under
verification; gate-only tests do not establish end-to-end shutdown correctness.

## Contract

`RuntimeDb::with_read_scope` derives a facade sharing the existing pool and
writer, but carrying a `RuntimeDbReadScope` for pooled read-role operations.
The scope has an absolute monotonic deadline and explicit cancellation.
Unscoped facades keep the existing acquisition-only budget: the default 250 ms
does not become an SQL execution deadline.

The acquisition deadline is the smaller of the existing acquisition budget and
the explicit scope deadline. Checks run before acquisition, before the business
closure, and before a successful result escapes. Asynchronous admission also
observes cancellation. A synchronous checkout can remain blocked until its
bounded acquisition deadline; it does not poll or open overflow connections.

Every read uses a child cancellation token. Dropping an asynchronous read future
cancels only that operation, not its parent scope or sibling reads. Once blocking
work starts, it owns its actual admission permit and reader lease until it exits,
even if its caller stops waiting. Cancellation cannot forcibly stop arbitrary
Rust, filesystem, or Git work; those phases require separate cooperation.

Only a scoped lease installs a SQLite progress handler. It observes cancellation
and the absolute deadline without issuing SQL or calling user code. The handler
is removed before protection/connection-cleanliness checks and before returning
the connection to the pool. Panic cleanup also removes it and discards the
connection. No connection-wide interrupt handle can outlive a lease and affect
the next borrower.

Protection-loss and broken-database errors take precedence over retryable scope
cancellation. An expired or cancelled read cannot return fresh success, even
when its closure catches an interrupted-SQL error or produces a value after
cancellation.

The scope does not change the writer, writer transactions, or the legacy
`RuntimeDb::connection` surface. Callers must propagate the scoped facade to
every intended pooled read; it is not thread-local or ambient authority.

## Follow-on Integration Boundaries

### Flight delivery

The gate's entries, last-good fallback, invalidation, and terminal delivery use
one state lock. A completed flight carries its immutable delivery, including
the fallback selected at that terminal decision. An awakened waiter never
looks up fallback bytes by key again: the key might already belong to a newer
flight or have been invalidated. If invalidation commits first, the old flight
is closed without fallback and cannot republish success or stale cache data.
If completion commits first, its delivery remains valid, but invalidation
removes the cached bytes. Flight identity prevents a late guard from removing
or closing a replacement.

### Owned build entry point

`run_owned` registers callers and their absolute deadlines under the gate state
lock. A caller's departure only cancels the shared scope when no live waiter
remains. The shared deadline is fixed at leader admission, independent of the
first caller and any later follower. A cancelled or expired flight closes
without selecting stale while its worker is still draining.

The same lock registers supervisors and `ProjectionBuildContext::blocking`
workers in a task set independent of cache entries. Each registered blocking
closure owns a shared leader lease before spawning, including while queued.
Closing or invalidating entries cannot release that capacity early. A
supervisor continues awaiting the same build future after cancellation; it
does not abort it. Panics are observed through the registry.

`close` atomically stops admission, closes flights, and cancels their read
scopes. `close_and_drain` joins registered tasks without a false timeout-based
reclamation. Cancelling a drain does not move or drop the registry, and a later
owner may resume it. This API alone does not wire service shutdown.

### Per-request capability preflight

`preflight` is a typed, uncached lane in the same gate, not another projection
cache or a shared verification result. Each request must pass its own capability
check before a cache hit, stale response, or flight join. It uses the caller's
original absolute deadline, never a renewed budget after verification.

Preflight has one fixed worker slot independent of the configured `B` build
leaders. The combined supervised-work limit is **B + 1**, not B or 2B. A full
preflight lane rejects even potential cache hits or followers with a retryable
503; full build capacity still allows verification and joining an existing
flight when the preflight slot is free. This does not add a configuration knob
or allow parallel preflight writer recovery.

Admission, scope registration, blocking-task registration, and close use the
same gate lock. A queued worker owns its real permit and exit guard before
spawning. Caller timeout or cancellation only cancels its read scope; the
worker's scope registration and permit survive until actual exit, including
writer recovery and panic. Close cancels all registered preflights, and the
same cancellation-safe drain joins them. No read interrupt is installed on a
writer, and an already-started recovery atomic section must finish its existing
commit or rollback.

Both snapshot routes perform scoped preflight after authentication and before
accessing the gate's cached bytes or flights. Protection and retryable read
errors retain their typed HTTP failure instead of being flattened into disabled
capabilities. The handshake retains its existing capability-loading behavior.

### HTTP ownership integration

The opt-in Host snapshot/list entry points take the same derived `RuntimeDb`
through synchronous SQL and shared assembly helpers. The list path preserves
cached identity filtering, canonical lineage-parent fallback, sorting and
per-member malformed-payload placeholders. Scoped loaded-runtime lookup returns
`Result<Option<RuntimeHandle>>`: a lock timeout is an error, never an unloaded
fallback. `check_read_scope` performs real protection checks before checking
cancellation without opening a reader; `wait_for_memory_lock` uses that
checkpoint before waiting and at every exit. It must receive only an in-memory
lock acquisition, never a future that performs SQL or spawns another worker.
An already registered blocking closure can use a captured Tokio `Handle` to
wait for these locks while the async runtime keeps driving timers/cancellation.
This is cooperative cancellation, not a hard wall-clock bound on synchronous
filesystem checks, standard locks, or serialization.

Between assembled rows, `check_read_cancellation` only checks the scope on the
healthy path; if cancelled, it performs the real protection check before
returning the cancellation error. Entry and successful exit keep full protection
checks, avoiding per-member filesystem probes on ordinary roster/list reads.

The opt-in state projection also preserves loaded memory state and projection
cache semantics. Its read-only `AppStorage` reconstruction points the store and
all read models at the same derived DB, without migrating, creating directories,
or installing a writer. Child observability, identity lookup and cache-miss
trigger reads run synchronously in that worker; memory locks alone use the
captured runtime handle. No nested async SQL task is created. The former
unscoped HTTP projection entry points remain only as test reference paths.
Other ordinary runtime APIs are unchanged.

Preflight workers record non-retryable failures with code/status before sending
their result: successful oneshot send is not consumption acknowledgement.
Caller cancellation after send must not silently erase a protection failure.
The worker preserves the caller's tracing dispatcher, and never logs payloads.
Events check the current dispatcher and event filter directly rather than
trusting a callsite interest cached under a different thread-local subscriber.

Shared projection builds need waiter registration, last-waiter departure,
flight closure, and result publication to share one linearization boundary.
They must not inherit the first caller's cancellation token. Their supervisor
must retain real capacity until any blocking work exits, and cancelled flights
must not publish fresh or stale cache entries.
All real blocking work must use registered ownership, not just a supervisor
around an API that can itself detach an internal worker. Scope propagation to
such internal workers is a separate integration requirement.

Each route establishes its absolute caller deadline after authentication and
before capability SQL or gate admission. List/state use a ten-second budget;
snapshots use their existing route limits, now including preflight and waiting.
A new shared build has its own fixed route budget, independent of its first
caller. Unsettled deadline failures use `projection_deadline_exhausted` and do
not select stale bytes. State's existing stale-response status and principal
isolation are unchanged. Read-side writer self-heal retains its atomic recovery
behavior under supervised blocking ownership.

TCP, localhost and Unix listeners share one gate. The authenticated shutdown
handler closes admission and drains its registry before shutting down the host.
The main server retains an `AppState` owner and drains on both successful and
failed server exit before stopping the memory indexer or diagnostics. Drain
waits for real worker completion, including writer recovery; it does not impose
a timeout that pretends capacity has been reclaimed. This does not establish
that every Unix connection task has been joined.

## Verification

`runtime_db::reader_pool_tests` exercises actual recursive SQLite cancellation,
deadline expiry, capacity-one reuse, late cancellation of a previous scope,
per-future cancellation isolation, blocking capacity retention, protection-loss
precedence, and transaction/panic cleanup. Existing acquisition-budget tests
remain authoritative for unscoped behavior.
The owned gate's SQL regression composes actual recursive SQLite interruption
with last-waiter departure, a blocked worker-exit barrier, capacity-one reuse
and a healthy subsequent build. HTTP regressions check capability revocation
against the same warm cache and shared listener close/drain behavior.
A real WAL writer-lock regression cancels a preflight caller during self-heal,
checks that another preflight remains rejected and drain remains pending, then
releases the lock and verifies the durable heal committed before drain returns.
A paused-clock gate regression composes a real capability read with a delayed
build and checks that preflight time does not renew the caller's deadline. This
is a gate/DB contract test, not a complete HTTP or listener-shutdown E2E test.
