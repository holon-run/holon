# Private Child Agent Status Via Local Control API

Decision:

- add `RuntimeHost::get_active_agent_for_local_operator` that resolves any active
  agent (public or private child) without a visibility filter, surfacing
  `NotFound` / `Archived` / `Stopped` as structured `PublicAgentError` variants
- route `GET /agents/{agent_id}/status` through the new method so the local
  control API can inspect private child agent summaries
- add an optional `agent_id` argument to the `AgentGet` tool that, when set,
  resolves the target agent through `RuntimeHostBridge::get_active_agent_for_local_operator`
- keep the public-visibility filter in place for the public-facing
  `get_public_agent` / `get_public_agent_for_external_ingress` paths
  (they still return `PublicAgentError::Private` -> HTTP 403)

Reason:

- the current local control API is a trusted operator surface (loopback TCP,
  Unix socket, or operator-controlled bearer token)
- private child agents are otherwise hard to observe end-to-end through
  existing operator/control surfaces
- issue #1742 explicitly defers full `Subject`/authorization modeling and
  multi-user remote access to follow-up work
