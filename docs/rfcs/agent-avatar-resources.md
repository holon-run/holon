# Agent avatar resources

Status: approved for implementation (2026-11-08), issue #3939.

## Contract

`GET /api/agents/{agent_id}/avatar` is an authenticated, read-only image
resource. It does not add avatar fields to Agent DTOs, store recipes, or
migrate existing Agents. The handshake capability is `agent_avatar_v1`.

| Parameter | Default | Allowed values |
| --- | --- | --- |
| `version` | `light-mech-1-vivid-2` | That exact version |
| `expression` | `neutral` | `neutral`, `focused`, `questioning`, `waiting`, `sleeping` |
| `format` | `png` | `png`, `svg` |
| `size` | `128` for PNG | `32`, `64`, `128`, `256`, `512`; forbidden for SVG |

Omission permanently selects this version, not a moving `latest`. Unknown,
repeated (including percent-decoded duplicates), or invalid parameters return
400 using the normal JSON error envelope. SVG has `viewBox="0 0 100 100"`.
Expressions are explicit pure drawing inputs: the server does not derive
them from lifecycle or run a turn.

Identity uses the existing JS Unicode-code-point FNV-1a mapping with wrapping
32-bit arithmetic and seed namespace `light-mech-1`. The `holonbot` brand
recipe remains stable. Display names do not participate. Identical IDs,
including across servers and deletion/recreation, yield identical artwork;
collisions are allowed and images are not identity or authorization proofs.

The existing vivid-2 JS renderer is the byte-level SVG reference, across all
five expressions. PNG is rasterized from that same SVG with a font-free Rust
renderer. No arbitrary SVG, user strings, scripts, `foreignObject`, fonts,
filesystem assets, or network resources enter the drawing. A visible geometry,
color, or identity-mapping change requires a new public version. PNG encoder
changes may change bytes and ETags without changing visible artwork.

## Access and lifecycle

Every request follows the existing HTTP session/origin/access checks, validates
the ID, and checks the current target identity before parsing drawing parameters,
cache lookup, or conditional response handling. Like the adjacent authenticated
operator Agent detail resource, authorized operators can read private children;
this is not a new public roster or anonymous resource. Existing not-found,
deleting, and deleted errors retain their existing HTTP/JSON mappings.

The identity lookup is the target-access linearization point. Nothing grants
access to a subsequent request: cache entries contain only pure drawing bytes,
not target existence, visibility, session state, or authorization. The existing
registry forbids reusing retired IDs within one event-log epoch; this resource
does not relax that rule. The same ID in an independent server/epoch still
produces identical artwork, without reusing any access decision.

The lookup does not acquire runtime state, scan AgentHomes, construct summaries,
start unloaded/stopped Agents, enqueue messages, or create WorkItems. The normal
remote-access transport and session-revocation policy remain authoritative.

## Bytes, cache, and admission

200 returns `image/png` or `image/svg+xml`, `X-Content-Type-Options: nosniff`,
a strong ETag from the actual bytes, and `Cache-Control: private, no-cache`.
`If-None-Match` supports lists, weak comparison, and `*`; a match returns 304
without a body, only after reauthorizing. Errors are JSON, never images.

The per-server image service holds an in-memory LRU cache bounded by both
16 MiB and 512 entries. An individual image is limited to 2 MiB. Keys include
public version, recipe combination, expression, format, size, and an internal
renderer revision. Different IDs with the same combination may reuse bytes,
but every request independently checks its target.

Cold builds share two slots (including SVG, so PNG is never above two).
No more than 16 additional requests may wait, including same-key followers.
Cache hits skip admission. Same-key cold requests coalesce. Admission happens
before blocking work is spawned. Saturation returns 503 with `Retry-After: 1`.
Cancellation removes the in-flight leader and wakes followers; a raster already
running retains its build slot until it actually finishes. Failed/cancelled
results are not cached, and image work never runs on an async executor thread.
There is no disk cache, external service, browser, or Node runtime dependency.

## Clients

The Web client uses the shared authenticated binary transport, not an
unauthenticated image URL or a URL containing credentials. It selects the API
only from an authenticated handshake advertising `agent_avatar_v1`; capability
absence is the old-daemon signal. Arbitrary 401/404/network failures do not
permanently disable the API. Unsupported servers and image failures use the
existing identical local renderer without blocking lists or losing expressions.

The shared SDK's `requestBinary()` returns the Blob, response headers, and
status while preserving the existing custom fetch, credentials, cancellation,
and JSON error behavior. `ApiHttpError` retains response headers, including
`Retry-After`; binary callers do not create a second authentication stack.

In-flight requests and Blob URLs belong to one server/authentication epoch.
Connection changes, logout, and authorization invalidation cancel that epoch,
release resources, and reject late results. Client cache reuse never proves
permission. Identical requests coalesce and object URLs are reference-counted
and revoked when no longer owned. Decorative images retain `alt=""` beside the
Agent name; raw SVG is never injected as HTML.

The client repository admits at most 128 entries and 8 MiB of cached Blobs,
with a 2 MiB per-image limit and four concurrent requests. Unreferenced entries
may be evicted; live object URLs are not evicted to admit another image.
When admission fails, the component keeps the local renderer. These bounds
describe retained resources, not the browser's total networking or decode memory.

## Verification and exclusions

Cross-language fixtures freeze identity and exact SVG bytes for Unicode,
overflow, empty input, hostile strings, brand artwork, and all expressions.
PNG tests cover dimensions, transparency, and alignment with the same SVG;
small-size visual inspection supplements semantic tests. HTTP tests cover
conditional reads, authentication/revocation, current target errors,
unloaded/stopped lifecycle, strict parameters, bounds, single-flight, failure,
and cancellation. Web tests cover capability negotiation, credential transport,
epoch isolation, and URL ownership.

Synthetic offline release benchmarks record cold SVG/PNG and hot-cache
latencies, image sizes, CPU/RSS where available, and saturation behavior.
Dependency/license, build size/time, and supported binary release targets must
be reported; unverified platforms are not implied to pass.

Native client UI (#3940/#3941), uploads, per-Agent customization, ASCII avatars,
merging, releasing, deploying, and daemon restart are outside this change.
