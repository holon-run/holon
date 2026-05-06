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

## v0.14 minimum closure

- `MemorySearch` indexes only governed memory/evidence sources:
  `agent_home/memory/self.md`, `agent_home/memory/operator.md`, workspace
  profiles, briefs, context episodes, and work items.
- Ordinary workspace Markdown remains a workspace file surface. It is inspected
  through file tools, not returned as Holon memory by default.
- `MemoryGet` accepts only opaque `source_ref` handles from the governed source
  set. Paths, URLs, query strings, and unknown prefixes are rejected or return
  not found; they are not retrieval authority.
- Memory results expose governance/provenance metadata so curated durable
  memory, workspace projections, and runtime evidence do not collapse into one
  trust class.
- Turn-local and provider remote compaction remain request/projection behavior.
  They can preserve bounded context and observability metadata, but they do not
  write curated durable memory.

## Deferred scope

1. Keep durable memory writes explicit and scoped to agent or workspace
   ownership.
2. Add a future explicit `MemoryWrite`/`Remember` contract only after the
   authority, target scope, and review/audit boundary are defined.
3. Keep agent-home `AGENTS.md` as durable agent behavior guidance, not a cache
   for project notes or task plans.

Tracked by #924 for the memory and compaction governance boundary.
