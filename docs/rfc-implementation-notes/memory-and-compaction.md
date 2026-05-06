# Memory and Compaction Implementation Notes

Related handles:

- `rfc-agent-and-workspace-memory`
- `rfc-long-lived-context-memory`
- `rfc-turn-local-context-compaction`
- `rfc-openai-remote-compaction`

## Current implementation posture

Holon has separate concepts for loaded guidance, curated memory, runtime
evidence, working-memory projection, and turn-local compaction. This separation
matches the RFC direction: compaction should prepare a bounded request
projection, not silently become durable memory.

The main risk is governance drift. If implementation notes, transient plans, or
tool traces are copied into durable memory without a clear rule, later turns can
confuse short-lived execution context with long-lived agent or workspace
knowledge.

## Open gaps

1. Keep durable memory writes explicit and scoped to agent or workspace
   ownership.
2. Keep compaction outputs traceable to source evidence and separate from
   curated memory.
3. Reconcile provider-specific remote compaction behavior with the local
   request-projection contract.
4. Add verification around memory search/retrieval boundaries so normal
   workspace Markdown, runtime evidence, and curated memory do not collapse into
   one trust class.
5. Keep agent-home `AGENTS.md` as durable agent behavior guidance, not a cache
   for project notes or task plans.

Tracked by #924 for the memory and compaction governance boundary.
