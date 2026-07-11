# OpenAI Incremental Continuation

OpenAI `previous_response_id` continuation is implemented as transport-local
request lowering, not as runtime prompt semantics.

The runtime still builds a complete provider turn request. The OpenAI transport
stores the previous request shape, full input items, response output items,
response id, and whether replay would omit provider-side context, scoped by the
prompt cache agent id and cache key. Requests without that stable scope do not
reuse a continuation snapshot. A later request uses `previous_response_id` when
its full input is a strict append-only extension of the previous request input
plus the locally visible previous response output.

Lineage eligibility and replay eligibility are separate. A response id remains
eligible for continuation even when the response contains provider-private
output that cannot be reconstructed locally. If a `previous_response_id`
request receives a 4xx response, the transport retries with its provider window
only when that replay is lossless. Otherwise it refuses the replay instead of
silently dropping server-side context.

Other continuation misses still send a full request: prompt or cache shape
changes, missing cache scope, tool schema changes, turn-local compaction,
missing response ids, or non-append conversation shape. Diagnostics set
`server_side_context_may_be_lost=true` when such a full request cannot preserve
known provider-side context.

Provider-specific wire constraints are applied only after continuation
eligibility is decided from the complete request shape. In particular, xAI
Responses requests use `store=true` so returned response ids remain available
for later continuation. An xAI continuation sends `previous_response_id`
without `instructions`, while initial and full fallback requests keep
`instructions`. Standard OpenAI Responses requests keep `store=false` and
`instructions` for both full and incremental requests.

Runtime events record secret-safe request-lowering diagnostics such as hit/miss
status, fallback reason, and possible server-side context loss, but they do not
expose response ids or make incremental continuation part of provider-neutral
prompt assembly.
