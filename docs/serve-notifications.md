# Serve Control Plane: Notification Stream Contract

This document describes the notification and event streaming contract for the `holon serve` control plane API, following the OpenAI Codex protocol pattern.

## Overview

The notification stream enables real-time, bidirectional communication between clients and the `holon serve` runtime. Notifications are JSON-RPC messages sent from server to client over a persistent NDJSON (Newline-Delimited JSON) connection.

### Protocol Reference

This implementation follows the OpenAI Codex App Server protocol:
- **Reference**: [openai/codex@91a3e17](https://github.com/openai/codex/blob/91a3e179607ae4cc23a3d80505bc3fee056704c7/codex-rs/app-server/README.md)
- **Transport**: HTTP with NDJSON streaming
- **Format**: JSON-RPC 2.0

## Connection Setup

### Request Headers

```
Accept: application/x-ndjson
Content-Type: application/x-ndjson
```

### Response Headers

```
Content-Type: application/x-ndjson
Cache-Control: no-cache
Connection: keep-alive
```

### Establishing a Stream

```bash
curl -N -H "Accept: application/x-ndjson" \
     -H "Content-Type: application/x-ndjson" \
     http://localhost:8080/rpc/stream
```

## Notification Types

### 1. Item Notifications (`item/*`)

Item notifications represent the lifecycle of items (work units, artifacts, results) within the system.

#### `item/created`

Emitted when a new item is created.

```json
{
  "jsonrpc": "2.0",
  "method": "item/created",
  "params": {
    "item_id": "item_123",
    "type": "created",
    "status": "pending",
    "content": {
      "text": "User's input or request",
      "metadata": {}
    },
    "timestamp": "2026-02-09T12:00:00Z",
    "thread_id": "thread_abc",
    "turn_id": "turn_xyz"
  }
}
```

#### `item/updated`

Emitted when an item is updated (e.g., status change, content modification).

```json
{
  "jsonrpc": "2.0",
  "method": "item/updated",
  "params": {
    "item_id": "item_123",
    "type": "updated",
    "status": "completed",
    "content": {
      "result": "Output from processing"
    },
    "timestamp": "2026-02-09T12:01:00Z",
    "thread_id": "thread_abc",
    "turn_id": "turn_xyz"
  }
}
```

#### `item/deleted`

Emitted when an item is deleted.

```json
{
  "jsonrpc": "2.0",
  "method": "item/deleted",
  "params": {
    "item_id": "item_123",
    "type": "deleted",
    "status": "deleted",
    "timestamp": "2026-02-09T12:02:00Z"
  }
}
```

### 2. Turn Notifications (`turn/*`)

Turn notifications represent the lifecycle of execution turns (processing cycles) within a thread.

#### `turn/started`

Emitted when a new turn begins processing.

```json
{
  "jsonrpc": "2.0",
  "method": "turn/started",
  "params": {
    "turn_id": "turn_123",
    "type": "started",
    "state": "active",
    "thread_id": "thread_abc",
    "started_at": "2026-02-09T12:00:00Z"
  }
}
```

#### `turn/completed`

Emitted when a turn completes successfully.

```json
{
  "jsonrpc": "2.0",
  "method": "turn/completed",
  "params": {
    "turn_id": "turn_123",
    "type": "completed",
    "state": "completed",
    "thread_id": "thread_abc",
    "started_at": "2026-02-09T12:00:00Z",
    "completed_at": "2026-02-09T12:01:00Z"
  }
}
```

#### `turn/interrupted`

Emitted when a turn is interrupted (e.g., pause, cancel).

```json
{
  "jsonrpc": "2.0",
  "method": "turn/interrupted",
  "params": {
    "turn_id": "turn_123",
    "type": "interrupted",
    "state": "interrupted",
    "thread_id": "thread_abc",
    "started_at": "2026-02-09T12:00:00Z",
    "message": "Turn interrupted: User requested cancellation"
  }
}
```

### 3. Thread Notifications (`thread/*`)

Thread notifications represent the lifecycle of conversation threads (controller sessions).

#### `thread/started`

Emitted when a new thread is created.

```json
{
  "jsonrpc": "2.0",
  "method": "thread/started",
  "params": {
    "thread_id": "thread_abc",
    "type": "started",
    "state": "running",
    "started_at": "2026-02-09T12:00:00Z"
  }
}
```

#### `thread/resumed`

Emitted when a paused thread is resumed.

```json
{
  "jsonrpc": "2.0",
  "method": "thread/resumed",
  "params": {
    "thread_id": "thread_abc",
    "type": "resumed",
    "state": "running",
    "started_at": "2026-02-09T12:00:00Z"
  }
}
```

#### `thread/paused`

Emitted when a thread is paused.

```json
{
  "jsonrpc": "2.0",
  "method": "thread/paused",
  "params": {
    "thread_id": "thread_abc",
    "type": "paused",
    "state": "paused"
  }
}
```

#### `thread/closed`

Emitted when a thread is closed.

```json
{
  "jsonrpc": "2.0",
  "method": "thread/closed",
  "params": {
    "thread_id": "thread_abc",
    "type": "closed",
    "state": "closed"
  }
}
```

## State Values

Common state values used across notifications:

| State | Description |
|-------|-------------|
| `active` | Currently processing |
| `completed` | Successfully finished |
| `interrupted` | Stopped before completion |
| `running` | Thread is actively running |
| `paused` | Temporarily suspended |
| `closed` | Terminated |

## Bidirectional Streaming

The stream supports both server-to-client notifications and client-to-server requests.

### Client Request Example

```json
{"jsonrpc":"2.0","id":1,"method":"holon/status","params":{}}
{"jsonrpc":"2.0","id":2,"method":"turn/start","params":{"thread_id":"thread_abc","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"请帮我分析这个 PR"}]}]}}
```

### Server Response Example

```json
{"jsonrpc":"2.0","id":1,"result":{"state":"running","events_processed":42}}
{"jsonrpc":"2.0","id":2,"result":{"turn_id":"turn_123","state":"active","started_at":"2026-02-09T12:00:00Z"}}
```

## End-to-End Flow Example

A complete session showing the notification flow:

```
Client: → {"jsonrpc":"2.0","id":1,"method":"thread/start","params":{}}

Server: ← {"jsonrpc":"2.0","method":"thread/started","params":{...}}
Server: → {"jsonrpc":"2.0","id":1,"result":{"thread_id":"thread_abc","session_id":"thread_abc",...}}

Client: → {"jsonrpc":"2.0","id":2,"method":"turn/start","params":{"thread_id":"thread_abc","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"请分析这个告警"}]}]}}

Server: ← {"jsonrpc":"2.0","method":"turn/started","params":{...}}
Server: → {"jsonrpc":"2.0","id":2,"result":{"turn_id":"turn_123","state":"active",...}}

Server: ← {"jsonrpc":"2.0","method":"item/created","params":{...}}
Server: ← {"jsonrpc":"2.0","method":"item/updated","params":{...}}

Server: ← {"jsonrpc":"2.0","method":"turn/completed","params":{...}}

Client: → {"jsonrpc":"2.0","id":3,"method":"turn/steer","params":{"turn_id":"turn_123","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"补充：重点看 flaky 测试"}]}]}}

Server: → {"jsonrpc":"2.0","id":3,"result":{"turn_id":"turn_123","state":"active","accepted_items":1,...}}
Server: ← {"jsonrpc":"2.0","method":"item/created","params":{...}}

Client: → {"jsonrpc":"2.0","id":4,"method":"turn/interrupt","params":{"turn_id":"turn_123"}}

Server: ← {"jsonrpc":"2.0","method":"turn/interrupted","params":{...}}
Server: → {"jsonrpc":"2.0","id":4,"result":{"turn_id":"turn_123","state":"interrupted",...}}

## `turn/start` Input Schema

`turn/start` requires:

- `thread_id` (string): target thread.
- `input` (array): one or more user message items.

Minimal example:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "turn/start",
  "params": {
    "thread_id": "thread_abc",
    "input": [
      {
        "type": "message",
        "role": "user",
        "content": [
          {
            "type": "input_text",
            "text": "请给我一个修复方案"
          }
        ]
      }
    ]
  }
}
```

## `turn/steer`

`turn/steer` appends user input to an in-flight turn.

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "turn/steer",
  "params": {
    "turn_id": "turn_123",
    "input": [
      {
        "type": "message",
        "role": "user",
        "content": [
          {
            "type": "input_text",
            "text": "补充上下文：只改 pkg/serve"
          }
        ]
      }
    ]
  }
}
```
```

## Integration Testing

The notification contract is covered by integration tests in `pkg/serve/notification_integration_test.go`:

### Test Coverage

1. **Notification Creation**
   - Item notifications (created, updated, deleted)
   - Turn notifications (started, completed, interrupted)
   - Thread notifications (started, resumed, paused, closed)

2. **JSON-RPC Conversion**
   - Valid notification-to-JSON-RPC conversion
   - Parameter marshaling/unmarshaling
   - Method naming (e.g., `item/created`)

3. **Stream Writing**
   - Single notification writes
   - Multiple notification sequences
   - NDJSON format validation

4. **Broadcasting**
   - Single subscriber
   - Multiple subscribers
   - Unsubscribe behavior
   - Concurrent broadcasts

5. **End-to-End Workflows**
   - Thread → Turn → Item lifecycle
   - Bidirectional request/response

### Running Tests

```bash
# Run all notification tests
go test -v ./pkg/serve/... -run TestNotification

# Run specific integration test
go test -v ./pkg/serve/... -run TestNotificationEndToEnd
```

## Method Naming Style

Following RFC-0005 and Codex protocol, all notification methods use **slash-style naming**:

- `item/created`, `item/updated`, `item/deleted`
- `turn/started`, `turn/completed`, `turn/interrupted`
- `thread/started`, `thread/resumed`, `thread/paused`, `thread/closed`

This style is consistent across:
- Notifications (server-to-client)
- Control methods (client-to-server): `thread/start`, `turn/start`, `turn/steer`, `turn/interrupt`
- Runtime methods: `holon/status`, `holon/pause`, `holon/resume`, `holon/logStream`

## Implementation Files

- **Types**: `pkg/serve/notification.go`
- **Stream Handler**: `pkg/serve/webhook.go` (`/rpc/stream`) and `pkg/serve/stream.go`
- **Integration Tests**: `pkg/serve/notification_integration_test.go`

## Regression Prevention

To prevent method naming/style drift:

1. **Tests enforce slash-style naming** - All notification methods are validated to use `/` separator
2. **JSON-RPC format validation** - All notifications must conform to JSON-RPC 2.0
3. **Constants for notification types** - Use defined constants (e.g., `ItemNotificationCreated`) instead of string literals

## See Also

- [RFC-0005: Serve API Direction](../rfc/0005-serve-api-direction.md) - Overall API design
- [docs/serve-webhook.md](serve-webhook.md) - Webhook ingress documentation
- [OpenAI Codex Protocol](https://github.com/openai/codex/blob/91a3e179607ae4cc23a3d80505bc3fee056704c7/codex-rs/app-server/README.md) - Reference implementation
