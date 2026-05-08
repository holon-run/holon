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

## Installing and Listing Skills

The HTTP control plane exposes skill endpoints:

- `GET /agents/:agent_id/skills` — List installed or available skills
- `POST /control/agents/:agent_id/skills/install` — Install a skill
- `POST /control/agents/:agent_id/skills/uninstall` — Remove a skill

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
- [Runtime Model](/concepts/runtime-model.md) — How skills fit into the agent
  operating loop
