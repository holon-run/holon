# RFC-0005: Serve API Direction (Codex-style Control Plane + Provider-Specific Ingress)

| Metadata | Value |
| :--- | :--- |
| **Status** | **Proposed** |
| **Author** | Holon Contributors |
| **Created** | 2026-02-08 |
| **Updated** | 2026-02-08 |
| **Parent** | RFC-0004 |
| **Issue** | [#600](https://github.com/holon-run/holon/issues/600) |

## 1. Summary

This RFC defines the API evolution direction for `holon serve` after the GitHub webhook MVP, establishing:

1. **Control-plane protocol**: Codex/OpenAI-style JSON-RPC for serve control and observability APIs
2. **Ingress path design**: Provider-specific webhook endpoints (e.g., `/ingress/github/webhook`)
3. **Deferred generic events API**: No `/v1/events` until multi-connector stabilization

## 2. Motivation

`holon serve` now supports GitHub webhook ingestion for local forwarding and controller-agent execution. We need a clear API evolution path that avoids premature standardization of generic event payloads and aligns with industry patterns for agent control surfaces.

### 2.1 Problems with Premature REST API

- Early REST surfaces often require breaking changes as protocols mature
- Generic event ingestion requires multi-connector validation to get right
- Control plane benefits from structured RPC semantics more than REST

### 2.2 Why Codex/OpenAI-Style Protocol

- Industry-standard pattern for agent control planes
- JSON-RPC provides structured request/response with clear error handling
- Supports streaming notifications for logs and status updates
- Well-defined schema can be referenced and code-generated

## 3. Goals and Non-Goals

### 3.1 Goals

1. Document Codex/OpenAI-style JSON-RPC as the control-plane protocol direction
2. Refactor GitHub webhook route to provider-specific path (`/ingress/github/webhook`)
3. Define minimal initial JSON-RPC method set (status/pause/resume/log-stream)
4. Explicitly defer `/v1/events` with clear rationale
5. Link API direction docs from proactive runtime documentation

### 3.2 Non-Goals

- No generic public event ingestion API in this phase
- No multi-provider connector implementation in this issue
- No full implementation of JSON-RPC methods (protocol direction only)

## 4. Design Decisions

### 4.1 Control Plane Protocol: JSON-RPC (Codex/OpenAI Style)

The control plane for `holon serve` will use JSON-RPC 2.0 following the OpenAI Codex protocol schemas.

#### Reference Implementation

Upstream commit (pinned for stability):
- `openai/codex@91a3e179607ae4cc23a3d80505bc3fee056704c7`

App Server protocol docs:
- https://github.com/openai/codex/blob/91a3e179607ae4cc23a3d80505bc3fee056704c7/codex-rs/app-server/README.md
- Schema generation: `codex app-server generate-json-schema --out DIR`

Key schema references:
- JSON-RPC envelopes: `JSONRPCRequest.json`, `JSONRPCResponse.json`
- Message types: `JSONRPCMessage.json`
- Protocol bundle: `codex_app_server_protocol.schemas.json`

#### Protocol Rationale

1. **Structured RPC semantics**: Clear method names, parameters, and error responses
2. **Bidirectional streaming**: Support for log streaming and status notifications
3. **Industry alignment**: Compatible with OpenAI's agent protocol patterns
4. **Schema-driven**: Types can be code-generated in multiple languages

### 4.2 Ingress Path Design: Provider-Specific Endpoints

Webhook ingress endpoints MUST follow provider-specific naming:

```
/ingress/<provider>/webhook
```

#### Examples

- `/ingress/github/webhook` - GitHub webhooks (current)
- `/ingress/gitlab/webhook` - GitLab webhooks (future)
- `/ingress/bitbucket/webhook` - Bitbucket webhooks (future)

#### Benefits

1. **Clear separation**: Ingress is distinct from control plane
2. **Provider-native**: Each adapter handles provider-specific payload format
3. **Future-proof**: Easy to add providers without API changes
4. **Independent evolution**: Each provider can have its own versioning

### 4.3 Deferred Generic Events API

**Decision**: Do NOT add `/v1/events` in this phase.

#### Rationale

1. **Premature standardization**: Generic event contract requires validation from 2+ connectors
2. **Unclear requirements**: Real-world usage will inform the right abstraction
3. **Avoid breakage**: Early generic APIs often require breaking changes
4. **Focus on MVP**: Provider-specific endpoints are sufficient for current needs

#### Revisit Criteria

Consider `/v1/events` only after:

1. **Multiple connectors implemented**: At least 2+ production connectors (GitHub + one other)
2. **Validated normalization**: Event contract proven in real usage across providers
3. **Clear use case**: Customer demand for unified event ingress beyond provider-specific

#### Future Design (Deferred)

When `/v1/events` is revisited, it should:

- Accept normalized `EventEnvelope` payloads
- Support provider-agnostic event types
- Include provider metadata for traceability
- Maintain compatibility with provider-specific endpoints

## 5. Minimal JSON-RPC Method Set (Initial)

The initial control-plane methods under a `holon/*` namespace:

### 5.1 Core Methods

| Method | Parameters | Returns | Description |
|--------|-----------|---------|-------------|
| `holon/status` | - | Status object | Current serve state (running/paused), stats |
| `holon/pause` | timeout? (optional) | Success confirmation | Pause event processing |
| `holon/resume` | - | Success confirmation | Resume event processing |
| `holon/logStream` | from_position?, lines? | Streaming log lines | Stream execution logs |

### 5.2 Method Definitions (Draft)

#### holon.getStatus

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "holon.getStatus",
  "params": {}
}
```

**Response:**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "state": "running",
    "events_processed": 42,
    "last_event_at": "2026-02-08T17:00:00Z",
    "controller_session_id": "sess_abc123"
  }
}
```

#### holon.pause

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "holon.pause",
  "params": {
    "timeout_seconds": 300
  }
}
```

**Response:**
```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "success": true,
    "message": "Paused for 300 seconds"
  }
}
```

#### holon.resume

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "holon.resume",
  "params": {}
}
```

**Response:**
```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "success": true,
    "message": "Resumed event processing"
  }
}
```

#### holon.logStream (Streaming)

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "holon.logStream",
  "params": {
    "from_position": 0,
    "max_lines": 100
  }
}
```

**Response (streamed):**
```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "stream_id": "stream_xyz",
    "logs": [
      {"level": "info", "time": "2026-02-08T17:00:00Z", "message": "Event received"},
      {"level": "info", "time": "2026-02-08T17:00:01Z", "message": "Controller triggered"}
    ]
  }
}
```

Follow-up notifications via server-sent events or WebSocket.

## 6. Implementation Plan

### Phase 1: Documentation + Path Refactoring (This Issue)

1. ✅ Create RFC-0005 documenting API direction
2. Refactor webhook route from `/webhook` to `/ingress/github/webhook`
3. Update documentation to reference provider-specific paths
4. Add forward compatibility note for control-plane methods

### Phase 2: JSON-RPC Control Plane (Follow-up Issue)

1. Implement JSON-RPC handler for `holon/*` methods
2. Add status/pause/resume endpoints
3. Add log streaming support
4. Generate TypeScript/Go types from Codex schemas

### Phase 3: Multi-Connector Validation (Future)

1. Implement second connector (e.g., GitLab)
2. Validate event normalization across providers
3. Revisit `/v1/events` design with real usage data

## 7. Migration Path

### 7.1 Webhook Path Migration

**Current:** `/webhook`
**New:** `/ingress/github/webhook`

Migration steps:
1. Add new route `/ingress/github/webhook` with current behavior
2. Keep `/webhook` as deprecated alias for 1-2 releases
3. Log deprecation warning when `/webhook` is used
4. Remove `/webhook` in future release

### 7.2 Control Plane Addition

New JSON-RPC endpoints will be added at a separate path (e.g., `/rpc` or `/control`), independent of ingress.

## 8. Risks and Mitigations

### 8.1 Risks

1. **Codex protocol changes**: Upstream schema may evolve
   - Mitigation: Pin to specific commit SHA, vendor schemas if needed

2. **Provider path confusion**: Users may expect generic `/webhook`
   - Mitigation: Clear documentation, deprecation period for old path

3. **Deferred `/v1/events` blocks use cases**: Potential future need
   - Mitigation: Document clear revisit criteria, keep design flexible

### 8.2 Mitigations

1. **Vendor schemas**: Copy JSON-RPC envelope schemas to `holon` repo if Codex evolves
2. **Documentation**: Link RFC from serve command help and docs
3. **Feedback loop**: Track user requests for generic events API

## 9. Open Questions

1. Should JSON-RPC endpoints be at `/rpc`, `/control`, or `/v1/rpc`?
2. Should `holon.logStream` use SSE, WebSocket, or chunked HTTP?
3. What authentication/authorization model for control-plane endpoints?
4. Should we vendor Codex schemas or reference them externally?

## 10. References

- OpenAI Codex App Server Protocol: https://github.com/openai/codex/blob/91a3e179607ae4cc23a3d80505bc3fee056704c7/codex-rs/app-server/README.md
- RFC-0004: Proactive Agent Runtime
- RFC-0003: Skill Artifact Architecture
- Issue #600: Serve API direction discussion
- Issue #573: GitHub webhook ingress epic

## 11. Follow-up Tasks

### Documentation

- [ ] Update `docs/serve-webhook.md` with new `/ingress/github/webhook` path
- [ ] Add control-plane section to `docs/serve.md` (new file)
- [ ] Link RFC-0005 from CLI help text

### Implementation (Future)

- [ ] Implement JSON-RPC handler skeleton
- [ ] Add `holon.getStatus` method
- [ ] Add `holon.pause` and `holon.resume` methods
- [ ] Add `holon.logStream` streaming support
- [ ] Add tests for control-plane endpoints
- [ ] Generate TypeScript/Go types from JSON schemas

### Multi-Connector (Future)

- [ ] Implement GitLab connector
- [ ] Validate event normalization
- [ ] Revisit `/v1/events` design
