# Runtime Host Activation Admission And Read Projections

## Status

Accepted for implementation by GitHub issue #2475.

## Problem

`RuntimeHost` historically exposed runtime handles to both read-only queries and
execution-bearing requests. A read such as transcript retrieval could therefore
create an agent runtime, start its scheduler loop, and claim durable input.

That hidden side effect also raced with shutdown: shutdown drained the currently
registered runtimes while a concurrent read could spawn and register a new one
after the drain. The new runtime was not included in shutdown settlement and
could leave a message permanently `dequeued` across restart.

## Contract

The host separates three operations.

### Durable read

A durable read validates agent identity and opens agent storage without loading,
waking, or otherwise creating a runtime. Read-only HTTP and RPC handlers use this
surface by default.

### Loaded-runtime observation

A loaded-runtime lookup returns an existing live `RuntimeHandle` or `None`. A
miss is not an activation request. Read projections may use an existing runtime
to enrich durable state with transient information.

### Execution activation

Runtime creation is an explicit admission operation with a reason such as
scheduler dispatch, operator control, external ingress, wake, startup recovery,
agent lifecycle, or child supervision. The reason is included in structured
runtime activation logs.

## Host registry state

One lock protects both the activation phase and the loaded-runtime map:

```text
HostRuntimeRegistry {
    phase: Open | Closing | Closed,
    agents: Map<AgentId, AgentEntry>,
}
```

Activation holds the registry write lock while it:

1. verifies that the phase is `Open`;
2. returns an existing live runtime or removes a finished entry;
3. creates the runtime and its task;
4. registers the entry.

Shutdown holds the same write lock while it changes `Open` to `Closing` and
drains every registered entry. It then requests shutdown and awaits or aborts
the drained tasks before setting `Closed`.

The lock is the linearization boundary:

- activation registered first is included in shutdown;
- shutdown closed admission first causes activation to fail without spawning.

Repeated shutdown is idempotent. Execution-bearing HTTP requests rejected after
admission closes return retryable `runtime_shutting_down` with HTTP 503.

## Read behavior during shutdown

Durable reads do not require activation and may continue while their storage
dependencies remain available. Execution ingress is rejected once the registry
enters `Closing`; this issue does not introduce a new durable mailbox acceptance
contract for requests received during shutdown.

Blocking task output is a live-runtime operation. If the target runtime is not
loaded, callers may read already persisted output without blocking, but a
blocking request returns a typed conflict instead of activating the agent.

## Startup recovery of orphaned claims

Startup recovery runs before listeners accept requests. It may change a queue
entry from `dequeued` to `interrupted` only when one transaction proves:

1. the queue entry is still `dequeued`;
2. no canonical activation owns the message;
3. no terminal turn exists for the message;
4. startup has not admitted live runtimes;
5. the target agent identity remains active.

The transition is compare-and-set and records an audit event. Any canonical
activation or terminal turn excludes the entry from orphan recovery. No age,
posture, or heuristic threshold is sufficient evidence.

Recovered non-stopped agents are explicitly activated with the startup recovery
reason before HTTP listeners open. Stopped agents retain the recoverable durable
claim but are not started.

## Verification

Tests cover:

- durable reads of unloaded agents without map mutation;
- activation and shutdown linearization in both orders;
- execution ingress after admission closes;
- orphan `dequeued` recovery and idempotence;
- canonical or terminal ownership exclusions;
- read-only HTTP routes without activation;
- unloaded blocking task-output behavior.

## Non-goals

- Waking or preloading agents for read convenience.
- Requeueing every `dequeued` entry.
- Changing canonical activation settlement semantics.
- Accepting shutdown-window ingress under a new delivery protocol.
- Introducing a general service or plugin framework.
