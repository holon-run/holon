# 097 Storage-Backed Agent State Projection

## Choice

Treat `GET /api/agents/{agent_id}/state` as a read projection that does not own
runtime activation.

`RuntimeHost` selects a loaded runtime when one already exists. Otherwise it
assembles the same HTTP snapshot from read-only agent storage and Runtime DB
read models. The projection reports its `loaded` or `storage` source through
diagnostics.

## Reason

Most `/state` fields are persisted facts or derived read models. Starting a
runtime merely to read them turns bootstrap, resume, and reconnect traffic into
an implicit lifecycle operation and multiplies that cost across the agent
roster.

## Preserved Boundary

The storage fallback never calls `get_or_create_agent` or `spawn_runtime`.
Explicit control, ingress, and scheduling paths remain responsible for runtime
activation. `ProjectionGate` still bounds and coalesces projection work
regardless of source.
