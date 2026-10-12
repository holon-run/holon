---
title: iOS cache-first offline reading
date: 2026-10-12
status: implementation
---

# iOS cache-first offline reading

## Contract

The iOS reader restores locally confirmed reading projections before attempting
network refresh. A failed connection, timeout, or refresh does not remove cached
content or prevent navigation. Offline reading covers cached agent lists, turn
summaries, previously loaded history, brief bodies, and reading positions.
Unfetched history, brief bodies, execution details, and files cannot be invented
offline. The UI identifies cached state and unavailable content.

This is not a second event ledger. The server conversation read model remains
authoritative. Only a successfully validated response may replace a projection.
History is merged only within the same event-log epoch. A new epoch invalidates
the previous conversation's bodies. Responses are fenced by connection,
partition, selected agent, and request generation.

## Persistence

Use a bounded, atomically replaced file in Application Support, excluded from
backup, with complete file protection. Migrate the previous Caches file after a
successful write. Credentials, staged login proofs, and expanded execution
details never enter the reading cache. A serial background worker performs disk
reads, encoding, validation of size bounds, and writes. Main-actor state owns
the in-memory projection; initialization cannot overwrite later mutations.

Keep at most twelve entries, including roster entries. Snapshot payloads are
bounded to 256 KiB. Each conversation holds at most eighty brief bodies, each
bounded to 256 KiB, with a 2 MiB aggregate budget; the file has a 16 MiB ceiling.
Evict older content when capacity is exhausted. Age alone does not delete
content during a prolonged outage; saved time expresses freshness, not online
authority. Missing or corrupt storage degrades to an empty cache, never a
network prerequisite or an unbounded read.

`load` updates the persisted LRU order without changing `savedAt`. Initialization
starts a shared background read; callers await `prepare()` before restoring
persisted projections. Memory-only caches are immediately ready.

`save` reports whether a projection passed the structural memory bounds.
Rejecting an oversized snapshot returns `false` and preserves the previous
entry; brief bodies are trimmed to their count and byte budgets instead.
Encoding and exact byte checks remain off the main actor. A snapshot that
expands beyond 256 KiB when encoded makes `flush()` return `false`, preserving
the previous file. The caller must reduce the snapshot window before retrying;
`save == true` is not a durability guarantee. Other persistence failures also
return `false`. Future-dated entries are rejected, but there is no age TTL.

## Reading context versus authority

Cached data is partitioned by API base, profile/network, runtime, user, and
visibility scope. Cold-start restoration uses a separately persisted, locally
confirmed reading context. A staged credential is never a confirmed context.
The context permits local reading only: it must not populate the live connection
identity, publish Share Extension capabilities, or enable sending, read markers,
report submission, or other server mutations.

Explicit logout, removal of a profile, switching accounts, and authoritative
401/403 responses withdraw the relevant context and invalidate its cache.
An unreachable node is not evidence of revoked authorization. Local content may
be stale; the client cannot discover remote revocation while disconnected.
Ordinary capacity eviction, OS storage failures, device lock, and uninstall
remain limits to availability; no secure-erasure promise is made.

## Refresh

Restore first, then refresh in the background. Selecting a conversation must not
wait for the roster request. Keep cached content visible during refresh and
preserve loaded history when the epoch remains unchanged. On successful
conversation bootstrap, prefetch a bounded tail of at most eight unloaded brief
bodies; SSE commits merge and persist without starting another prefetch.
Visible-body and activity requests
share the same two-slot expansion budget. Returning to a previously opened conversation restores its bodies
without waiting for the node.

History retention remains capacity-bounded. A merged window with more than
180 closed turns falls back to the confirmed server live window for display.
If the displayed snapshot exceeds 256 KiB when encoded, persistence tries the
confirmed same-epoch live window instead; it never invents a pagination cursor.
If that live window also exceeds the encoding limit, the old cache is preserved.
Cold restart restores only the retained window, not every previously loaded
history page.

Manual retry and foreground reentry attempt revalidation. Restored reading
context and authenticated transports have distinct lifetimes; stale callbacks
must not reenable a previous account or overwrite another conversation.

## Verification

Hosted XCTest covers persistence, migration, corruption, bounded eviction,
partition isolation, initialization/mutation ordering, cache-first restoration,
failed refresh, same-epoch history, changed epochs, stale responses, cold-start
connection failure, logout, and authorization denial. Simulator UI tests cover
reading cached conversations across restart and node unavailability. These
tests do not establish real-device startup latency, radio behavior, or
filesystem durability under process termination. Complete file protection is
asserted on device only; simulator persistence tests do not validate device
Data Protection attributes.
