# `/agents/list` batch posture projection

Decision:

- serve `/agents/list` from one `RuntimeDb` read that collects every roster
  member's posture facts through a single connection inside one deferred
  read transaction
- keep the posture rules in one pure function shared with the per-agent
  read model, instead of a list-only copy

Reason:

- the old path opened one SQLite connection per small query per agent
  (N+1), amplifying schema init/close and mutex contention until list
  requests hit the 10s projection timeout (#3641)
- one shared rule function keeps the batched response field-identical to
  the per-agent projection, so batch and single-agent views cannot drift

Preserved boundary:

- resident runtimes are served from the same committed read view as
  unloaded members: the list never starts a runtime and never mixes
  per-agent read views; per-agent runtime state stays available through
  the agent state endpoints
- unreadable per-member payloads degrade that member to placeholder facts
  (the previous per-agent behavior) instead of failing the whole roster
