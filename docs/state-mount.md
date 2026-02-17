# State Directory (`agent_home/state`)

## Overview

Holon provides a persistent state directory at:

- Host path: `<agent-home>/state`
- Container path: `${HOLON_STATE_DIR}`

This directory is used for cross-run mutable state, such as skill caches and runtime metadata.

## What State Is For

Use state for data that is useful to reuse but safe to regenerate:

- API caches (issues/PR snapshots, sync caches)
- Incremental indexes
- Cursor/checkpoint files
- Runtime/session metadata used by serve

Do not treat state as the primary place for final deliverables.

## What Not To Store In State

Do not store final artifacts in `${HOLON_STATE_DIR}`:

- final patches intended for publish
- final summaries/reports intended for handoff
- canonical source-of-truth files that belong in workspace

Use output/workspace for final artifacts, and state for mutable caches/runtime state.

## Lifecycle

- State persists across runs when you reuse the same `--agent-home`.
- Temporary agent homes imply temporary state.
- Deleting `<agent-home>/state` is safe (cache cold start), but may lose serve runtime metadata.

## CLI Usage

### Stable state across runs

```bash
holon run --goal "Analyze project trends" --agent-home ~/.holon/agents/analysis
```

### Isolated ephemeral state

```bash
holon run --goal "One-off task"
```

(Without `--agent-home`, run may use a temporary agent home depending on command defaults.)

## Skill Author Contract

Skills should treat `${HOLON_STATE_DIR}` as optional but preferred for persistence.

Guidelines:

1. Namespace by skill name.
- Example: `${HOLON_STATE_DIR}/project-pulse/issues-cache.json`

2. Handle first-run and empty state.
- Missing files/directories should not fail the skill.

3. Keep state non-deterministic and regenerable.
- Cache data and cursors belong here.

4. Use atomic writes for important files.
- Write temp file then rename.

## Serve Runtime Notes

For `holon serve`, the same state root is also used for runtime metadata and diagnostics (for example startup diagnostics and subscription status). This enables restart continuity and troubleshooting.

## Troubleshooting

### Permission error on state writes

Ensure `<agent-home>/state` is writable by the Holon process.

### Stale cache behavior

Clear selected cache subdirectories under `${HOLON_STATE_DIR}` and rerun.

### Unexpected state sharing

Use distinct `--agent-home` values for different agents/projects.

## Related Docs

- `README.md`
- `docs/skills.md`
- `docs/serve-webhook.md`
