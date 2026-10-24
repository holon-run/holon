# Scoped Runtime DB Reads

## Status

Stage 3A: opt-in pooled-reader infrastructure. HTTP request deadlines and
shared projection-flight cancellation are not implemented by this change.

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

Shared projection builds need waiter registration, last-waiter departure,
flight closure, and result publication to share one linearization boundary.
They must not inherit the first caller's cancellation token. Their supervisor
must retain real capacity until any blocking work exits, and cancelled flights
must not publish fresh or stale cache entries.

HTTP integration must include capability checks and loaded-runtime reads in
the original caller's remaining budget. Read-side writer self-heal must retain
its atomic recovery behavior under supervised blocking ownership. State's
existing stale-response status and principal isolation are unchanged.

## Verification

`runtime_db::reader_pool_tests` exercises actual recursive SQLite cancellation,
deadline expiry, capacity-one reuse, late cancellation of a previous scope,
per-future cancellation isolation, blocking capacity retention, protection-loss
precedence, and transaction/panic cleanup. Existing acquisition-budget tests
remain authoritative for unscoped behavior.
