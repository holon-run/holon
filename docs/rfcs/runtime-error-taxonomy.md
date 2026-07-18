# Runtime Error Taxonomy

## Status

Accepted for incremental implementation by GitHub issue #2269.

## Problem

Holon already has typed provider failures and selected typed runtime database
conflicts, but many runtime, tool, and HTTP boundaries still collapse errors
into free-form strings. That loses stable codes, retryability, safe source
context, and the IDs needed to follow a failure from an input message through a
turn, provider/tool/task work, and the final brief or failure artifact.

The runtime continues to use `anyhow::Error` as an internal propagation
container. This RFC defines one typed description extracted at boundaries; it
does not require replacing every internal result type.

## Contract

`RuntimeErrorDescriptor` contains:

- `domain`: a small stable classification, not a module name.
- `code`: the existing stable string code when one exists.
- `retryable`: whether repeating the same operation may succeed without a
  semantic input change.
- `operator_message`: a bounded, redacted summary safe for operator surfaces.
- `recovery_hint`: an optional bounded corrective action.
- `safe_context`: allowlisted scalar identifiers and status fields.
- `source_chain`: a bounded, deduplicated, redacted display chain.

Initial domains are:

`runtime`, `storage`, `policy`, `io`, `conflict`, `not_found`, `validation`,
`provider`, `tool`, `task`, `http`, and `unknown`.

Unknown errors are fail-closed: code `runtime_error`, domain `unknown`, and
`retryable=false`.

`RuntimeErrorContext` contains correlation references:

- `message_id`
- `turn_id`
- `run_id`
- `work_item_id`
- `tool_execution_id`
- `task_id`
- `correlation_id`
- `causation_id`
- `provider`
- `model_ref`

These values identify existing records. They do not copy record payloads and do
not create a second tracing identity system.

## Classification order

The descriptor extractor walks the `anyhow` source chain and selects the most
specific supported typed source:

1. `RuntimeError`
2. `ToolError`
3. `ProviderTransportError`
4. `RuntimeStateTransitionConflict`
5. `RuntimeDbRetryableError`
6. `std::io::Error`
7. conservative unknown fallback

Provider attempt timelines may enrich context, but free-form timeline messages
do not create a stable code. Upstream provider codes such as
`context_length_exceeded` are stored on the typed transport error and queried
directly.

## Boundary projections

### Tool

`ToolError` retains `kind`, `message`, `details`, `recovery_hint`, and
`retryable`. Additive `domain` and `source_chain` fields carry taxonomy data.
`ToolError::from_anyhow` preserves an existing nested `ToolError`; otherwise it
projects the shared descriptor.

Canonical tool failure output and audit events retain the structured
`ToolError`. Existing model-visible receipt fields remain compatible.

### HTTP and local client

The HTTP error envelope retains required `ok`, `error`, and optional `code` and
`hint`. It additively exposes:

- `domain`
- `retryable`
- `context`
- `correlation`

Typed status mapping is:

| Domain | HTTP status |
| --- | --- |
| `validation` | 400 |
| `not_found` | 404 |
| `conflict` | 409 |
| `policy` | 403 |
| retryable `storage` / `io` | 503 |
| retryable `provider` | 503 |
| non-retryable `provider` | 502 |
| other / unknown | 500 |

Endpoint-specific existing statuses and codes remain valid. The generic
boundary mapping is used where an `anyhow::Error` reaches `error_response`.
The local client reads all new fields but continues to accept old envelopes.

Task and work-item lifecycle handlers must produce typed domain errors before
the HTTP boundary. HTTP code must not depend on `starts_with`, `ends_with`, or
`contains` over an error message.

### Failure artifacts

`FailureArtifact` retains its existing category, kind, summary, provider,
model, status, task, exit status, source chain, and metadata fields. It
additively exposes:

- `domain`
- `retryable`
- `recovery_hint`
- `context`

Provider, runtime, and task failure artifacts project the same taxonomy and
reuse the existing message, turn, run, work-item, tool, task, provider, and
model identifiers. Additive serde defaults keep old persisted JSON readable.
State bootstrap projections must bound all added strings.

## Redaction and bounds

No public descriptor may serialize raw `Debug` output, request/response bodies,
SQL, environment variables, authorization headers, bearer tokens, capability
URLs, URL credentials or queries, or arbitrary absolute paths.

The implementation:

- allowlists `safe_context` keys;
- removes URL credentials, query, and fragment;
- replaces absolute path prefixes with a redacted marker and basename;
- redacts messages containing known authorization or token markers;
- bounds each public string and the number of source-chain entries;
- deduplicates source-chain entries after redaction.

High-cardinality correlation values may appear in records, artifacts, audit
payloads, and structured logs. They must not become metric labels.

## Persistence

This contract uses additive serde fields in existing JSON records and evidence.
No SQLite schema migration is required. Existing `MessageEnvelope`,
`TurnRecord`, `ToolExecutionRecord`, `TaskRecord`, audit events, and brief
records already contain the required identifiers or adjacency.

## Compatibility

- Existing string codes remain unchanged.
- Existing required JSON fields remain unchanged.
- New fields use serde defaults and omission when empty.
- Provider retry count, fallback order, and wire behavior are unchanged.
- Old clients can ignore new fields; new clients accept old envelopes.
- Existing metadata remains readable, but taxonomy facts should not only exist
  in metadata.

## Non-goals

- Replacing all `anyhow` usage.
- Defining a plugin or registry system for errors.
- Rewriting provider retry policy.
- Adding distributed tracing or a vendor observability backend.
- Adding high-cardinality metric labels.
- Performing the `types.rs` or storage facade refactors tracked separately.

## Verification

Tests must cover:

- typed classification through multiple `anyhow::Context` layers;
- conservative unknown retryability;
- source-chain bounds, deduplication, URL/path/secret redaction;
- ToolError compatibility and typed runtime/provider projections;
- typed task/work-item HTTP status and local client decoding;
- provider upstream code detection without message substring matching;
- runtime, provider, and task failure artifact taxonomy and correlation fields;
- state bootstrap bounds for added artifact fields.
