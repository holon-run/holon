# /status Remains Agent-Facing While /state Stays Bootstrap-Oriented

Decision:

- keep `GET /agents/:id/status` as the concise agent-facing summary surface
- keep `GET /agents/:id/state` as the richer first-party projection bootstrap
- reuse the same `AgentSummary` contract inside `/state.agent`

Reason:

- first-party projection clients need a coherent bootstrap payload
- operators and scripts still benefit from a smaller summary surface
