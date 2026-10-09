# AI content reports

Holon exposes a protected write-only endpoint for reporting user-visible AI
responses:

```http
POST /api/content-reports
Authorization: Bearer <authenticated session>
Content-Type: application/json
```

```json
{
  "agent_id": "agent_...",
  "turn_id": "turn_...",
  "message_id": "transcript_...",
  "category": "harmful_or_abusive",
  "description": "optional explanation",
  "client_request_id": "retry-key"
}
```

`message_id` is the public conversation activity identifier for an assistant
transcript entry, not client-supplied content. The endpoint accepts only active
public agents and operator-visible assistant rounds. Runtime-private
checkpoints, tool output, incoming messages, and missing or inaccessible
targets are all returned as the same `404 not_found` response.

Supported categories are `harmful_or_abusive`, `sexual_content`,
`hate_or_harassment`, `self_harm`, `violence`, `privacy`, and `spam_or_other`.
The request body is limited to 32 KiB; identifiers are limited to 256 Unicode
scalars, descriptions to 2,000, and client request IDs to 128 ASCII
letters/numbers plus `.`, `-`, or `_`. A principal may create at most 10
reports per rolling hour. Retries with the same principal, target, and
`client_request_id` return the original report; requests without an idempotency
key are deduplicated by principal, target, and category.

Successful first submissions return `201`:

```json
{
  "report_id": "report_...",
  "status": "accepted",
  "created_at": "2026-10-09T16:00:00.000Z"
}
```

Reports persist a bounded text snapshot and its hash, along with the target
references and server-derived source metadata. They are not emitted through
conversation activity or SSE, and this version provides no report query,
withdrawal, moderation, or notification endpoint.
