---
title: Skills guide
summary: Reusable SKILL.md workflows, skill locations, and how to develop custom skills.
order: 40
---

# Skills Guide

Skills are reusable local workflows. A skill is rooted at a `SKILL.md` file and
describes a repeatable task pattern that an agent can load when needed.

## What a Skill Is

A skill typically contains:

- **Purpose** — What task the skill helps with
- **Workflow** — Step-by-step guidance for the agent
- **Boundaries** — What the skill should and should not do
- **References** — Related files or commands to inspect

Skills are **not automatically active**. The runtime exposes a catalog of
available skills, and the agent opens the relevant `SKILL.md` only when a
matching task appears.

## Location

Repository-local skills commonly live under:

```text
.codex/skills/<skill-name>/SKILL.md
skills/<skill-name>/SKILL.md
```

Example skills in this repository include:

- `ghx`
- `github-issue-solve`
- `github-pr-fix`
- `github-review`

## How Agents Use Skills

1. The runtime exposes a **skills catalog** with name, description, and path
2. The agent chooses a skill only if it matches the current task
3. The agent reads the skill's `SKILL.md`
4. The agent follows the workflow with normal tool calls

This keeps skills explicit and avoids loading irrelevant guidance.

## Managing skills with the CLI

Holon separates skill management into two layers:

- **Skill Library** — A global catalog of known skills managed with
  `holon skills add/remove/check/reconcile/catalog`. Think of it as your
  local skill registry.
- **Agent Skills** — Per-agent enablement managed with
  `holon skills enable/disable/list`. A skill must exist in the library
  before it can be enabled for an agent.

### Skill Library (global)

The Skill Library is your local catalog of skills. Manage it with:

#### List the library catalog

```bash
holon skills catalog
```

Shows all skills registered in the local Skill Library.

#### Add a skill to the library

```bash
# Add from a local directory or SKILL.md file
holon skills add /path/to/skill-dir

# Add from a remote source
holon skills add https://github.com/user/repo/tree/main/skills/my-skill --remote

# Add a built-in skill by name
holon skills add my-skill --builtin

# Copy the skill into the user directory instead of referencing it
holon skills add /path/to/skill --copy
```

#### Remove a skill from the library

```bash
holon skills remove my-skill
```

#### Check library consistency

```bash
# Check all library entries against .skill-lock.json
holon skills check

# Check a specific skill
holon skills check my-skill
```

#### Reconcile library with lock file

```bash
# Reconcile all library entries
holon skills reconcile

# Reconcile a specific skill
holon skills reconcile my-skill
```

### Agent Skills (per-agent)

Once a skill is in the library, enable it for a specific agent:

#### List enabled skills for an agent

```bash
# List for the default agent
holon skills list

# List for a specific agent
holon skills list --agent reviewer
```

Shows all skills currently enabled for the agent, including their name,
scope (agent, workspace, or user), and source.

#### Enable a skill for an agent

```bash
# Enable for the default agent
holon skills enable my-skill

# Enable for a specific agent
holon skills enable my-skill --agent reviewer

# Enable and copy into the agent home
holon skills enable my-skill --copy
```

#### Disable a skill for an agent

```bash
# Disable for the default agent
holon skills disable my-skill

# Disable for a specific agent
holon skills disable my-skill --agent reviewer
```

> **Compatibility aliases:** `holon skills install` and
> `holon skills uninstall` are still accepted but map to the new
> add/enable and remove/disable model. Prefer the new commands for
> clarity.

### Skill source types

| Source | Flag | Example |
|--------|------|---------|
| Local path | (default) | `holon skills add ./skills/my-skill` |
| Remote URL | `--remote` | `holon skills add https://... --remote` |
| Built-in | `--builtin` | `holon skills add ghx --builtin` |

### Command summary

| Layer | Command | Purpose |
|-------|---------|---------|
| Library | `holon skills catalog` | List library catalog |
| Library | `holon skills add <source>` | Add a skill to the library |
| Library | `holon skills remove <name>` | Remove from the library |
| Library | `holon skills check [name]` | Check lock-file consistency |
| Library | `holon skills reconcile [name]` | Reconcile with lock file |
| Agent | `holon skills list [--agent]` | List enabled skills |
| Agent | `holon skills enable <name>` | Enable for an agent |
| Agent | `holon skills disable <name>` | Disable for an agent |

## Managing skills via HTTP

The HTTP control plane separates library and agent operations:

### Library endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/api/skills/catalog` | List library catalog |
| `GET` | `/api/skills/catalog/{skill_id}` | Get skill detail |
| `POST` | `/api/skills/catalog/add` | Add a skill to the library |
| `POST` | `/api/skills/catalog/remove` | Remove from the library |
| `POST` | `/api/skills/catalog/reconcile` | Reconcile with lock file |
| `POST` | `/api/skills/catalog/check` | Check consistency |

### Agent endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/agents/:agent_id/skills` | List agent skills |
| `POST` | `/control/agents/:agent_id/skills/enable` | Enable for agent |
| `POST` | `/control/agents/:agent_id/skills/disable` | Disable for agent |

> **Deprecated:** `POST /control/agents/:agent_id/skills/install` and
> `POST /control/agents/:agent_id/skills/uninstall` remain for
> compatibility but are superseded by enable/disable and
> add/remove.

## TUI integration

The Terminal UI provides skill management alongside the CLI:

- **Slash commands** — Type `/skills` to view agent skills,
  `/skill-catalog` to browse the library, `/skill-add <source>` to add
  to the library, `/skill-remove <name>` to remove, and
  `/skill-enable <name>` / `/skill-disable <name>` to manage agent
  enablement directly from the chat input.
- **Skill name completion** — The TUI auto-completes skill names when
  using slash commands.
- **Agent status sidebar** — The agent detail view under "Skills" shows
  all discoverable skills with their scope (agent, workspace, user).

These TUI features let you manage skills without leaving the interactive
session.

## Writing a Good Skill

Keep skills:

- **Small** — Focus on one workflow
- **Actionable** — Give concrete steps the agent can execute
- **Scoped** — Avoid broad project-overriding instructions
- **Durable** — Encode reusable behavior, not one-off task notes

Good skill topics:

- GitHub issue solving
- PR review workflow
- Release checklist
- Incident triage
- Project-specific test/debug loop

Poor skill topics:

- Temporary meeting notes
- One-off task plans
- Large copied documentation dumps

## Relationship to AGENTS.md

Use:

- **`AGENTS.md`** for durable role, authority, and local conventions
- **Skills** for reusable workflows
- **Work items** for current tracked objectives

These are different layers:

| Surface | Purpose |
|---------|---------|
| `AGENTS.md` | Standing instructions and boundaries |
| `SKILL.md` | Reusable workflow for a type of task |
| Work item | Current objective and progress |

## See Also

- [Multi-Agent Collaboration](/guides/multi-agent.md) — Delegating work to
  child agents
- [Work Items Guide](/guides/work-items.md) — Tracking durable objectives
- [Web GUI](/guides/web-gui.md) — Skill management in the browser
- [TUI Guide](/guides/tui.md) — Skill management in the terminal
- [Runtime Model](/concepts/runtime-model.md) — How skills fit into the agent
  operating loop
