# Managed Skills Workflow

Skills are local workflows rooted at `SKILL.md`. The skills catalog in prompt context lists discoverable skills by name, description, and file path without preloading their bodies; open a matching skill's `SKILL.md` before following it and read only the references needed for the current task.

## Installing & Enabling Existing Skills

Use the managed `holon skills` CLI rather than manually copying or symlinking skill directories or editing `.skill-lock.json` directly:

1. Import a skill into the local Skill Library:
   `holon skills add <source>` (use `--remote` when importing a remote source, `--skill <name>` to select one skill from a multi-skill source, and `--copy` only when an independent managed copy is explicitly required).
2. Enable a Library skill for the target agent:
   `holon skills enable <name>` (omit `--agent` for self in agent mode, or pass `--agent <id>` when authorized).
3. `holon skills install <name_or_path>` and `holon skills uninstall <name>` remain compatibility aliases for enabling/importing and disabling skills.

## Inspection, Updates, and Consistency Checks

- `holon skills list` — verify skills enabled and effective for the target agent.
- `holon skills catalog` — list entries in the local Skill Library catalog.
- `holon skills update [name]` — update supported remote Skill Library entries using `.skill-lock.json` metadata.
- `holon skills check [name]` — check Skill Library and lock-file consistency.
- `holon skills reconcile [name]` / `holon skills refresh` — reconcile lock state or rescan local skill roots.
- `holon skills disable <name>` / `holon skills remove <name>` — disable a skill for an agent or remove it from the Library.

## Linking, Scope, and Safety Rules

- Keep the default linked mode unless an independent managed copy (`--copy`) is explicitly needed; a local link reflects source-directory changes automatically without mutating its upstream repository.
- Check `--help` before acting when source format, scope, or flags are uncertain.
- Skill Library mutations are host-wide rather than scoped to one agent, and `--agent` never elevates caller authority.
- If `holon skills` is unavailable or rejects an operation, report the limitation instead of falling back to manual directory copying or stripping `HOLON_CALLER_*` provenance.
- This managed-installation rule governs installing/enabling existing skill packages; it does not prohibit normal authoring or editing of a repository-owned `SKILL.md` in an active workspace.
