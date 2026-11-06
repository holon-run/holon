# Preserve rejected recovery evidence during the fixed upgrade

The episode continuity upgrade may encounter an already persisted provider
recovery whose source linkage, predecessor selection or binding fails current
recovery validation. Such a directive is rejection evidence, not authority to
adopt its asserted WorkItem or source episode. Failing the entire migration
prevents unrelated agents from starting even when the recovery is already
terminal history reachable through an execution attempt.

At the fixed upgrade boundary, retain the Message digest and original payload,
but stop ownership expansion for a typed recovery continuity mismatch. Do not
rewrite legacy Turn owners, change recovery WorkItem metadata, select an
ambiguous predecessor, or grant a successful recovery result. SQL/read failures,
malformed canonical payloads, resource bounds and ordinary current-reader
validation still fail. Runtime recovery admission uses its unchanged strict
resolver and must continue rejecting the directive.

This is limited to the migration's explicit table-inventory capture; a normal
current closure cannot use that inventory to forgive a mismatch or adopt late
raw history. Independently proven references retain their ordinary closure.

The same fixed boundary retains terminal Wait bodies without treating their
old Turn/trigger links as live continuation roots. Otherwise a completed WorkItem
with repeated resolved waits recursively imports its entire old wait loop and
can exhaust the unchanged 64-edge bound. Active and Triggered waits still follow
their continuation edges; independently rooted Turns and Messages are still
captured. Work checkpoint capture uses the same distinction. Normal current
readers retain their existing strict closure and resource errors.

A pre-Turn interrupted claim whose exact source is Quarantined is not itself a
live upgrade root when the matching Interrupted outcome is present, no Run,
canonical Turn, assignment, current State binding or recovery successor exists,
and source generation/fences match. Its durable records are unchanged and no
claim-only Turn proof is minted. An independent current reference still selects
the Attempt through the strict closure and qualification checks. Missing tables
or witnesses, live queues and inconsistent canonical projections cannot use this
exclusion.

Event history ACK adoption streams keyset pages inside the original Current
upgrade transaction. Each page retains the existing record and byte bounds;
individual oversized bodies and invalid sequences still fail. An agent's total
archived history can span multiple pages without dropping old ACKs or qualifying
those Events as live Current. Digest checks, scope and the one-time cut remain
unchanged.

The historical Brief linkage backfill added `created_event_seq` to the payload
without updating its pre-link content hash. Original fixed Briefs can verify
that exact old hash by omitting only the serialized additive linkage field
from the digest stream. This requires unchanged fixed-seed metadata in the
current incarnation, matching SQL/payload linkage, and the same-scope creating
audit event. It does not rewrite hashes, seed fingerprints, bodies or Events.
Fresh records, late raw history, changed text/linkage, missing Events and other
hash mismatches retain strict rejection. A prepared alias still freezes the
full actual raw payload digest, and existing byte preflight/materialization
bounds remain unchanged.
