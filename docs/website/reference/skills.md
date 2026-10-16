---
title: Skills
summary: Skill sources, CLI commands, and API endpoints for installation, enablement, updates, and verification.
order: 34
---

# Skills

This page records skill sources, management commands, and API endpoints.
For workflows, see [Install and update skills](/guides/use-skills.md) and
[Write a skill](/guides/write-skill.md).

## What a Skill Is

See [Skills and tools](/concepts/skills-and-tools.md) for discovery, loading,
and authority boundaries. The entry file is `SKILL.md`.

## Location

Repository-local skills commonly live under:

```text
.codex/skills/<skill-name>/SKILL.md
skills/<skill-name>/SKILL.md
```

Example skills in this repository include:

- `ghx`
- `code-review`
- `github-issue-solve`
- `github-pr-fix`
- `github-review`

## How Agents Use Skills

The catalog exposes name, description, and path; the agent reads `SKILL.md`
when a task matches. Follow [Write and verify a skill](/guides/write-skill.md)
to check both loading and behavior.

## Managing skills with the CLI

Library commands affect the shared registry; enable/disable commands select an
agent's entries. The [management guide](/guides/use-skills.md) covers the
install → enable → verify workflow.

### Skill Library (global)

Commands in this section manage shared library state.

#### List the library catalog

`holon skills catalog` lists registered entries.

#### Add a skill to the library

`holon skills add <source> [--remote] [--skill <name>] [--copy]` accepts a local
directory or `SKILL.md`, a remote URL, or the GitHub shorthand below.
`--skill` selects a skill from a source with multiple entries. Default local
installation is linked; `--copy` creates an independent managed copy.

GitHub remote skills are fetched as a single repository archive and discovered
locally. This avoids depending on the GitHub Contents/Tree API for public
repositories; the archive is validated before any selected skill directory is
installed. Existing lock metadata still records the source, ref, path, and
folder hash used by `holon skills update`.

#### Remove a skill from the library

`holon skills remove <name>` removes a library entry.

#### Check library consistency

`holon skills check [name]` compares one or all entries with `.skill-lock.json`.

> **Note:** Installing an already-installed global skill is idempotent —
> `holon skills add` skips the install and reports the existing entry without
> error. This is safe to repeat in scripts and automation.

#### Reconcile library with lock file

`holon skills reconcile [name]` reconciles one or all entries with lock state.
Inspect differences before using it to accept changed contents.

#### Update skills from remote sources

`holon skills update [name]` fetches remote skill sources and compares against the lock file
(`.skill-lock.json`). Skills that have changed upstream are downloaded
and updated. The update preserves the npx `skills` v3 lock format
(source, subdir, mode, etag) for interoperability.

### Agent Skills (per-agent)

`--agent <id>` selects the target agent. In agent mode, omitting it targets the
caller; operator mode uses the default agent.

#### List enabled skills for an agent

`holon skills list [--agent <id>]` shows effective skills, including their name,
scope (agent, workspace, or user), and source.

#### Enable a skill for an agent

`holon skills enable <name> [--agent <id>] [--copy]` enables a known skill.
`--copy` places an independent managed copy in the agent home.

#### Disable a skill for an agent

`holon skills disable <name> [--agent <id>]` removes agent-level enablement.

> **Compatibility aliases:** `holon skills install` and
> `holon skills uninstall` are still accepted but map to the new
> add/enable and remove/disable model. Prefer the new commands for
> clarity.

### Skill `uses` shorthand

When adding a GitHub-hosted skill, you can use the `uses` shorthand syntax
instead of the repository URL:

```bash
# Full URL form
holon skills add https://github.com/holon-run/holon/tree/main/skills/ghx --remote

# uses shorthand — same result
holon skills add holon-run/holon/skills/ghx@main
```

The `uses` form (`owner/repo/path@ref`) is normalized to the full GitHub URL
internally. It also accepts `owner/repo/path#ref` as a compatible input.

### Skill source types

| Source | Flag | Example |
|--------|------|---------|
| Local path | (default) | `holon skills add ./skills/my-skill` |
| Remote URL | `--remote` | `holon skills add https://... --remote` |

### Command summary

| Layer | Command | Purpose |
|-------|---------|---------|
| Library | `holon skills catalog` | List library catalog |
| Library | `holon skills update` | Fetch and update skills from remote sources |
| Library | `holon skills refresh` | Refresh runtime catalog by rescanning local roots |
| Library | `holon skills add <source>` | Add a skill to the library |
| Library | `holon skills remove <name>` | Remove from the library |
| Library | `holon skills check [name]` | Check lock-file consistency |
| Library | `holon skills reconcile [name]` | Reconcile with lock file |
| Agent | `holon skills list [--agent]` | List enabled skills |
| Agent | `holon skills enable <name>` | Enable for an agent |
| Agent | `holon skills disable <name>` | Disable for an agent |

> **Note:** The `--builtin` flag and `kind = "builtin"` in template manifests
> have been removed in v0.26.0. Official Holon skills are referenced the same
> way as any other GitHub-hosted skill.

## Managing skills via HTTP

The HTTP control plane separates library and agent operations:

### Library endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/api/skills/catalog` | List library catalog |
| `GET` | `/api/skills/catalog/{skill_id}` | Get skill detail |
| `POST` | `/api/skills/catalog/add` | Add a skill to the library |
| `POST` | `/api/skills/catalog/refresh` | Refresh runtime catalog |
| `POST` | `/api/skills/catalog/remove` | Remove from the library |
| `POST` | `/api/skills/catalog/reconcile` | Reconcile with lock file |
| `POST` | `/api/skills/catalog/check` | Check consistency |
| `POST` | `/api/skills/catalog/update` | Update skills from remote sources |

### Agent endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/agents/:agent_id/skills` | List agent skills |
| `POST` | `/control/agents/:agent_id/skills/enable` | Enable for agent |
| `POST` | `/control/agents/:agent_id/skills/disable` | Disable for agent |

> **Deprecated:** `POST /api/control/agents/:agent_id/skills/install` and
> `POST /api/control/agents/:agent_id/skills/uninstall` remain for
> compatibility but are superseded by enable/disable and
> add/remove.

### Non-blocking install jobs

When a skill is installed through the HTTP API (e.g., from the Web
GUI), the install runs as a background job instead of blocking the
request. Job progress is available at `GET /api/jobs/{job_id}` and
persists across page reloads via localStorage.

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

Follow [Write and verify a skill](/guides/write-skill.md) for a minimal
`SKILL.md`, a test task, and checks for discovery and loading. Keep each skill
focused on one reusable workflow, such as review, release checks, incident
triage, or a project's test/debug loop. Temporary plans and copied document
dumps belong elsewhere.

## Relationship to AGENTS.md

See [Agents and guidance](/concepts/agents.md),
[Skills and tools](/concepts/skills-and-tools.md), and
[WorkItems and Tasks](/concepts/work-items-and-tasks.md) for standing
instructions, reusable workflows, and tracked objectives respectively.

## See Also

- [Multi-Agent Collaboration](/guides/delegate-work.md) — Delegating work to
  child agents
- [Work Items Guide](/reference/work-items.md) — Tracking durable objectives
- [Web GUI](/guides/use-web-gui.md) — Skill management in the browser
- [TUI Guide](/reference/tui.md) — Skill management in the terminal
- [Runtime Model](/concepts/runtime-model.md) — How skills fit into the agent
  operating loop
