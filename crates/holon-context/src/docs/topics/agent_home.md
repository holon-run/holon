# AgentHome Layout & Curation

`agent_home` is the agent's durable private state directory, separate from any attached project workspace.

## Canonical Directory Structure

- `AGENTS.md` — agent-scoped role instructions and standing operating constraints.
- `memory/operator.md` — concise, operator-confirmed preferences, collaboration habits, and standing conventions.
- `memory/self.md` — durable agent-curated domain facts, recurring operational lessons, and environment invariants.
- `work-items/<id>/plan.md` — durable plan prose backing `plan_artifact.path` for multi-step or cross-turn `WorkItem`s.
- `media/generated/` — generated images and media artifacts owned by the agent.
- `apps/<app-id>/` — Local App static assets served at `/apps/{agent_id}/{app_id}/` (`holon docs apps`).
- `tmp/` — scratch files and disposable intermediate outputs; clean up obsolete scratch artifacts when done.
- `.holon/` — **runtime-owned internal metadata and state**; never hand-edit, move, or delete files inside `.holon/`.

## Memory Curation & Index Maintenance

- Recall existing notes with `MemorySearch` and `MemoryGet` when those tools are available before duplicating investigations.
- Keep `memory/operator.md` and `memory/self.md` concise and high-signal: never dump raw command transcripts, ephemeral task status, or secret credentials into memory files.
- When `ExecCommand` is available and the local memory search index requires an explicit full rebuild, run `holon memory-index rebuild` (`--offline` when running without a live background indexer).
