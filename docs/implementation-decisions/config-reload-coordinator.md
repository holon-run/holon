# Host-Owned Coalesced Config Reload

Runtime config and credential mutations persist their durable source of truth
before they request reload. The HTTP request then returns without waiting for
all loaded runtimes to rebuild provider state.

Reload is owned by one host-lifecycle task. A monotonically increasing
generation records every accepted mutation, while a single worker serializes
reload rounds and coalesces bursts. If a mutation arrives during a round, the
worker runs again from the latest files before becoming idle. Shutdown cancels
and joins the worker.

This preserves three boundaries:

- request success means durable persistence plus an accepted reload obligation;
- only one reload round may publish runtime configuration at a time;
- handler cancellation cannot detach or partially own host lifecycle work.

Extending client timeouts would retain request latency and cancellation risk.
Spawning from each handler would allow older snapshots to finish after newer
ones. Parallel per-runtime fan-out is intentionally deferred: background
serialization removes request latency without introducing an unbounded
provider-rebuild resource spike.
