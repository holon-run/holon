---
title: RFC: Runtime ID Generation
date: 2026-05-29
status: draft
Handle: rfc-runtime-id-generation
---

# RFC: Runtime ID Generation

## Summary

Holon should use a small, explicit ID generation policy instead of scattering
raw UUID generation across runtime modules.

Runtime object IDs that are frequently shown to the model, operator, logs,
source refs, task handles, and branch/path names should be short, opaque, and
type-prefixed. Capability-bearing IDs and external bearer tokens should remain
high-entropy and unguessable. User- or integration-provided names should stay
as stable slugs rather than being folded into the random ID system.

The policy should be centralized in one runtime module and adopted gradually.
Existing stored IDs remain valid. Public APIs should continue to treat IDs as
opaque strings and must not promise a specific generated format.

## Problem

Holon currently generates IDs in multiple ways:

- plain UUID strings
- `prefix_` plus UUID simple strings
- `prefix-` plus UUID simple strings
- ad hoc prefixes in individual modules
- user-facing names that are sometimes mixed with generated IDs

This has several costs:

- long UUIDs consume prompt and transcript budget when they appear in tool
  receipts, task handles, source refs, WorkItem records, event history, and
  branch names
- inconsistent prefixes make logs and debug output harder to scan
- scattered generation makes it unclear which IDs are safe to shorten
- security-sensitive callback or trigger tokens can be accidentally discussed
  as if they were ordinary runtime object IDs
- tests can become coupled to incidental UUID formatting rather than runtime
  semantics

Holon needs a policy that reduces model-visible noise without weakening
capability security or forcing a disruptive storage migration.

## Goals

- make generated runtime object IDs shorter and easier to read
- keep IDs opaque at API and storage boundaries
- preserve strong entropy for capability-bearing or externally guessable tokens
- centralize ID generation behind named functions
- keep prefixes consistent by runtime object kind
- allow old UUID-shaped IDs to remain readable forever
- let tests assert semantic properties rather than exact UUID formats

## Non-goals

- do not migrate historical ledgers, task records, memory source refs, or
  workspace records just to normalize ID shape
- do not shorten bearer capabilities, callback URLs, webhook secrets, or any
  ID whose possession authorizes external action
- do not make ID format a stable public contract
- do not introduce a global counter for durable runtime object IDs
- do not specify sequence/index fields for append-only logs or child records;
  those should be designed separately from runtime object ID generation
- do not replace stable user-facing slugs such as agent IDs, template IDs,
  provider IDs, or skill IDs with random generated IDs
- do not require one large mechanical PR to replace every generation point

## ID Classes

Holon should classify generated identifiers before choosing an ID format.

### Runtime object IDs

Runtime object IDs identify records or handles inside Holon. They may be shown
often to the model or operator, but they are not bearer secrets.

Examples:

- message ID
- task ID
- run ID
- tool execution ID
- workspace ID
- WorkItem ID
- brief ID
- transcript entry ID
- episode ID
- wait condition ID
- timer ID
- delivery summary ID

These IDs should use compact prefixed random IDs:

```text
<prefix>_<short-random>
```

Examples:

```text
msg_3f8nq0za
task_9x4k2p7q
run_6kq1v2hd
tool_b7m2x9cw
ws_c7q2v9mk1p4a
work_p4a8r1mz
ep_z91xka7d
wait_k8p3f0na
timer_m2x7q4ac
```

The random part should be long enough for local runtime uniqueness, but not as
verbose as a UUID. A first-pass target is about 60 bits of randomness encoded
with a URL- and shell-friendly alphabet.

### Capability and secret IDs

Capability or secret IDs are IDs where possession, guessing, or leakage can
authorize future action or external ingress.

Examples:

- external trigger callback capability tokens
- wake URLs
- webhook secrets
- operator delivery tokens
- any URL or token accepted by an external system as bearer authority

These IDs must remain high entropy. New capability-bearing tokens should use at
least 128 bits of randomness, such as 26 or more base32 characters or an
equivalent longer random token. Existing UUID-simple capability tokens may
remain valid for compatibility, but new capability generators should not treat a
UUID-simple payload as satisfying a 128-bit floor.

The runtime may still give these IDs readable prefixes such as `cb_`, but the
prefix is diagnostic only. The random payload remains capability-grade.

### Stable names and slugs

Some identifiers are names rather than generated runtime records.

Examples:

- `agent_id`
- provider ID
- model ID
- template ID
- skill ID
- user-provided workspace names

These should not be regenerated, shortened, or normalized by the random ID
generator. Their contract is name stability, not random uniqueness.

Holon should avoid global auto-incrementing IDs for primary runtime objects.
They are short, but they introduce shared counter state, recovery rules,
cross-agent collisions, merge ambiguity, and guessability. Short random
prefixed IDs provide most of the token savings without central counter
coordination.

## Prefix Policy

Runtime object prefixes should be short, lower-case, and consistent.

Suggested initial prefixes:

| Object kind | Prefix |
| --- | --- |
| message | `msg` |
| task | `task` |
| run | `run` |
| tool execution | `tool` |
| workspace | `ws` |
| work item | `work` |
| brief | `brief` |
| transcript entry | `tr` |
| episode | `ep` |
| wait condition | `wait` |
| timer | `timer` |
| delivery summary | `deliv` |
| occupancy | `occ` |

Prefixes are for operator and developer readability. They are not a parsing
contract for external clients. Runtime code should not infer authorization or
object type from a prefix alone.

## Random Alphabet And Length

Runtime object IDs should use a compact alphabet that is safe in common Holon
surfaces:

- JSON strings
- Markdown
- shell commands
- URLs and callback metadata
- filesystem paths
- git branch names
- model-visible source refs

A base32-style lower-case alphabet is a good default:

```text
0123456789abcdefghjkmnpqrstvwxyz
```

This avoids visually confusing characters such as `i`, `l`, and `o`, while
remaining easy to copy.

Recommended first-pass lengths:

| Class | Randomness | Example length |
| --- | ---: | --- |
| runtime object | about 60 bits | 12 base32 chars |
| very short display-only handle | derived, not canonical | case by case |
| capability / bearer token | at least 128 bits | 26+ base32 chars or equivalent |

The canonical runtime object ID should be the short prefixed ID. Holon should
not rely on separate hidden UUIDs unless a specific subsystem needs them.
Model-visible receipts should render the canonical runtime object ID directly
rather than introducing a second display-handle mapping.

## Central Generator Module

ID creation should be centralized, for example in `src/ids.rs`.

The first implementation can stay simple:

```rust
pub fn message_id() -> String;
pub fn task_id() -> String;
pub fn run_id() -> String;
pub fn tool_execution_id() -> String;
pub fn workspace_id() -> String;
pub fn work_item_id() -> String;
pub fn brief_id() -> String;
pub fn transcript_entry_id() -> String;
pub fn episode_id() -> String;
pub fn wait_condition_id() -> String;
pub fn timer_id() -> String;
pub fn delivery_summary_id() -> String;

pub fn capability_id(prefix: &str) -> String;
```

Typed newtypes can be added later if a boundary benefits from stronger typing.
The first step should prefer a small function surface over a broad ID
framework.

## Compatibility

Holon must treat IDs as opaque strings.

Readers should continue accepting historical UUID-shaped IDs and older prefixed
IDs. Stored ledgers, task files, WorkItem records, transcript entries, memory
indexes, workspace records, and source refs should not be rewritten solely to
normalize ID shape.

New records may use the new generator while old records keep their original
IDs. Mixed-format histories are expected during migration.

Public documentation should say:

- IDs are stable references for the lifetime of the referenced object
- IDs are opaque
- clients must not depend on UUID shape, prefix spelling, or random length
- capability URLs or tokens must be handled as secrets regardless of display
  shape

## Migration Plan

Adopt the policy incrementally.

1. Add `src/ids.rs` with short runtime ID helpers and capability-grade helper.
2. Replace the highest token-cost runtime object generation points first:
   - task ID
   - tool execution ID
   - message ID
   - run ID
   - workspace ID
   - WorkItem ID
3. Replace lower-frequency runtime object IDs:
   - brief ID
   - transcript entry ID
   - episode ID
   - wait condition ID
   - timer ID
   - delivery summary ID
   - occupancy ID
4. Leave callback, external trigger, wake URL, and webhook capability tokens on
   the high-entropy path.
5. Update tests to assert semantic properties:
   - non-empty
   - correct object association
   - uniqueness for generated samples
   - old UUID-shaped inputs remain accepted where records are read
   - optional prefix checks only where the prefix is part of Holon's own
     generated output

## Testing Expectations

Tests should avoid asserting UUID format for runtime object IDs.

Good assertions:

```text
id is not empty
id starts with "task_" for newly generated task IDs
two generated IDs differ
the task record can be retrieved by the returned task ID
an old UUID-shaped task ID in a fixture still deserializes
```

Poor assertions:

```text
id parses as UUID
id length is exactly 36
all IDs use the same separator
external clients may parse object type from the prefix
```

Capability-token tests should instead assert sufficient entropy length and
non-disclosure behavior where possible.

## Proposed Principle

Holon runtime object IDs should be opaque, short, type-prefixed, and locally
unique. The first implementation should use one canonical short ID shape with a
`_` separator and a 12-character base32 random payload, and receipts should show
that canonical ID directly. Capability IDs and external bearer tokens should be
opaque, long, and unguessable. Stable user-facing slugs should stay names, not
random IDs. Sequence/index design is intentionally out of scope for this RFC.
