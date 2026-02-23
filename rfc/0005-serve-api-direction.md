# RFC-0005: Serve API Direction (Codex-style Control Plane + Provider-Specific Ingress)

| Metadata | Value |
| :--- | :--- |
| **Status** | **Draft** |
| **Author** | Holon Contributors |
| **Created** | 2026-02-08 |
| **Updated** | 2026-02-23 |
| **Parent** | RFC-0004 |
| **Issue** | [#600](https://github.com/holon-run/holon/issues/600) |

## 1. Summary

This RFC defines the API evolution direction for `holon serve` after the GitHub webhook MVP, establishing:

1. **Control-plane protocol**: Codex/OpenAI-style JSON-RPC for serve control and observability APIs
2. **Ingress path design**: Provider-specific webhook endpoints (e.g., `/ingress/github/webhook`)
3. **Deferred generic events API**: No `/v1/events` until multi-connector stabilization

## Implementation Reality (2026-02-23)

- This document is still a direction RFC, but a substantial subset is implemented.
- Control-plane JSON-RPC is available at `/rpc` (plus `/rpc/stream`) with:
  - `holon/status`, `holon/pause`, `holon/resume`, `holon/logStream`
  - `thread/start`, `turn/start`, `turn/steer`, `turn/interrupt`
- GitHub ingress is provider-specific at `/ingress/github/webhook`.
- Legacy `/webhook` has been removed; requests now return `404 Not Found`.
- Generic `/v1/events` remains deferred until multi-connector validation.

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
- No attempt to implement the full upstream Codex method surface in this issue

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

### 5.0 Scope Alignment with OpenAI/Codex Protocol

OpenAI/Codex app-server protocol defines a broad method surface (thread/turn lifecycle, item notifications, config/model/skills discovery, account/auth, feedback, MCP OAuth, platform-specific notifications).

Holon does **not** need to implement the entire upstream method set in one phase.

Implementation strategy:

1. Implement a **compatible subset** first (protocol envelope + core serve runtime operations).
2. Expand only when required by concrete Holon product needs.
3. Keep provider ingress independent from control-plane method expansion.

Method groups and Holon stance:

- **Core session/turn/event flow (required early)**:
  - `thread/start`, `thread/read` (or `thread/list`)
  - `turn/start`, `turn/interrupt`
  - notification flow for `item/*`, `turn/completed`, `thread/started` (or equivalent lifecycle notifications)
- **Runtime capability/discovery (optional early)**:
  - `config/read`, `model/list`, `skills/list`
- **Platform-coupled methods (deferred)**:
  - `account/*`, `feedback/upload`, `mcpServer/oauth/*`, and other provider-specific platform methods

### 5.1 Core Methods

| Method | Parameters | Returns | Description |
|--------|-----------|---------|-------------|
| `holon/status` | - | Status object | Current serve state (running/paused), stats |
| `holon/pause` | timeout? (optional) | Success confirmation | Pause event processing |
| `holon/resume` | - | Success confirmation | Resume event processing |
| `holon/logStream` | from_position?, lines? | Streaming log lines | Stream execution logs |

### 5.2 Method Definitions (Draft)

Method naming convention follows Codex/OpenAI protocol style:
- Use slash-delimited verbs/nouns (e.g., `thread/start`, `config/read`).
- For Holon control-plane methods, use `holon/<action>` names.

#### holon/status

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "holon/status",
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

#### holon/pause

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "holon/pause",
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

#### holon/resume

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "holon/resume",
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

#### holon/logStream (Streaming)

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "holon/logStream",
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

## 6. Implementation Status

### Completed in/around issue #600

1. ✅ RFC-0005 documents API direction and rationale.
2. ✅ Provider-specific ingress route is implemented at `/ingress/github/webhook`.
3. ✅ JSON-RPC control plane subset is implemented (`holon/*` + core thread/turn methods).
4. ✅ Webhook legacy alias `/webhook` has been removed.
5. ✅ `/v1/events` remains explicitly deferred.

### Remaining for future phases

1. Multi-provider connector expansion and cross-provider normalization validation.
2. Decide schema vendoring/codegen strategy against pinned upstream references.
3. Revisit `/v1/events` only after multi-connector stabilization criteria are met.

## 7. Migration Path

### 7.1 Webhook Path Migration (Completed)

**Removed:** `/webhook`  
**Supported:** `/ingress/github/webhook`

Current behavior:
1. Requests to `/webhook` return `404 Not Found`.
2. GitHub webhook ingress must target `/ingress/github/webhook`.

### 7.2 Control Plane Addition

Control-plane endpoints are available at `/rpc` and `/rpc/stream`, independent of ingress.

## 8. Risks and Mitigations

### 8.1 Risks

1. **Codex protocol changes**: Upstream schema may evolve
   - Mitigation: Pin to specific commit SHA, vendor schemas if needed

2. **Provider path confusion**: Users may still target removed `/webhook`
   - Mitigation: Clear documentation and migration guidance to `/ingress/github/webhook`

3. **Deferred `/v1/events` blocks use cases**: Potential future need
   - Mitigation: Document clear revisit criteria, keep design flexible

### 8.2 Mitigations

1. **Vendor schemas**: Copy JSON-RPC envelope schemas to `holon` repo if Codex evolves
2. **Documentation**: Link RFC from serve command help and docs
3. **Feedback loop**: Track user requests for generic events API

## 9. Open Questions

1. Should JSON-RPC endpoints be at `/rpc`, `/control`, or `/v1/rpc`?
2. Should `holon/logStream` use SSE, WebSocket, or chunked HTTP?
3. What authentication/authorization model for control-plane endpoints?
4. Should we vendor Codex schemas or reference them externally?

## 10. References

- OpenAI Codex App Server Protocol: https://github.com/openai/codex/blob/91a3e179607ae4cc23a3d80505bc3fee056704c7/codex-rs/app-server/README.md
- RFC-0004: Proactive Agent Runtime
- RFC-0003: Skill Artifact Architecture
- Issue #600: Serve API direction discussion
- Issue #573: GitHub webhook ingress epic

## 11. Follow-up Tasks

### Near-term

- [ ] Define authentication/authorization model for control-plane endpoints.
- [ ] Decide whether to vendor Codex schemas or keep external pinned references only.
- [ ] Clarify long-term endpoint versioning strategy (`/rpc` vs versioned RPC path).

### Multi-Connector (Future)

- [ ] Implement at least one additional connector (for example GitLab).
- [ ] Validate normalized event contracts across connectors in real usage.
- [ ] Revisit `/v1/events` only if the revisit criteria in Section 4.3 are met.
