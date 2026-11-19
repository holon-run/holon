---
title: Create and customize a dedicated role agent with permission boundaries
summary: Spin up a role agent from a template or select an existing agent in the Web GUI, persist standing responsibilities and approval gates in AGENTS.md and operator memory through natural conversation, and verify boundaries with a read-only dry run.
order: 20
---

# Create and customize a dedicated role agent with permission boundaries

Use this guide when you want a long-lived agent with a clear job description and explicit guardrails—so it can run routine read-only checks or isolated worktree edits autonomously while pausing for your approval before high-impact actions.

- For installing templates from remote sources or bootstrapping a fresh agent via CLI, see [Create an agent from a template](create-agent.md).
- For how agent guidance, workspace guidance, and trust levels interact, see [Agents, roles, and guidance](/concepts/agents.md).

## Prerequisites

- A running Holon runtime with the [Web GUI](use-web-gui.md) open and a working model route ([Configure models](configure-models.md)).
- Either a role template in your catalog (such as `code-reviewer`, `docs-steward`, or `research-steward`) to create a new agent, or an existing idle agent you want to tailor (wait for active tasks to finish before editing its rules).

## Steps

### 1. Create a role agent or open an existing one in the Web GUI

1. Open the [Web GUI](use-web-gui.md) Dashboard or **Agent Templates** (`/templates`).
2. To create a new specialist agent, choose an installed role template (for example `code-reviewer`, `docs-steward`, or `research-steward`) and give it a descriptive unused ID.
3. To customize an existing agent instead, select it from the Dashboard roster and confirm no active WorkItem is mid-edit.

Once created, an agent owns its own `AGENTS.md`, memory, and work history. Do not delete an active agent home or reapply a template over an existing agent just to adjust its rules.

### 2. Define responsibilities, approval gates, and preferences in conversation

Open the agent's conversation in the Web GUI, attach any target repository if needed, and state its standing contract in plain language:

> "Attach `/path/to/my-repo` as a workspace. Record these standing rules in your `AGENTS.md`:
> 1. You may inspect files, run local tests, and prepare draft patches in an isolated Git worktree autonomously.
> 2. Never merge PRs, push to `main`, publish external posts, or modify credentials without my explicit confirmation in chat.
> 3. Also save in your operator memory (`memory/operator.md`) that I prefer concise bullet summaries with verified command outputs."

The agent updates two agent-local files without touching runtime-managed `.holon/` state:

- **`AGENTS.md`**: Standing role, authorized actions, approval gates, and escalation rules—loaded automatically on every turn.
- **`memory/operator.md`**: Stable personal preferences such as reply language, formatting habits, and workflow defaults (see [Manage agent memory](manage-agent-memory.md)).

A well-structured `AGENTS.md` keeps autonomous actions separate from gated ones:

```markdown
## Responsibilities
- Audit pull requests and documentation changes in attached repositories.
- Reproduce issues and run local test suites in an isolated Git worktree.

## Authority
May run without repeated confirmation:
- Read files, inspect git history, and run local verification commands.
- Prepare draft patches or review notes inside a dedicated worktree.

Requires explicit operator confirmation:
- Pushing branches, merging PRs, creating releases, or posting externally.
- Any action involving credentials, billing, or irreversible external state.
```

### 3. Keep boundaries clean across scopes

Avoid duplicating the same rule across multiple files. Put each rule where its scope belongs:

| Rule type | Where it lives | Why |
|---|---|---|
| Standing role & approval gates | Agent home `AGENTS.md` | Follows the agent across every repository it works on |
| Repo build/test commands & code style | Repository `AGENTS.md` | Follows the codebase when that workspace is active |
| Reply language & style preferences | `memory/operator.md` | Curated personal preferences without bloating `AGENTS.md` |
| Reusable step-by-step procedures | [Skills](write-skill.md) (`SKILL.md`) | Loaded on demand when a task matches that workflow |
| Active task scope & checklist | Current `WorkItem` | Ephemeral per-objective plan rather than permanent policy |

> **Trust and isolation note:** Agent `AGENTS.md` is a runtime-loaded behavioral contract, not an OS-level hard sandbox. Local execution runs on the host (`host_local`), and project or external text (such as issue comments or fetched web pages) carries lower trust than operator instructions—it must never override approval gates written in `AGENTS.md`. See [Trust boundaries](/concepts/trust-boundaries.md).

### 4. Verify the updated contract with a read-only dry run

Before delegating real changes, run two quick checks in the same chat:

1. **Confirm loaded responsibilities:**

   > "Read your current `AGENTS.md` and `memory/operator.md`. Summarize what you can do autonomously versus what requires my approval, and inspect the attached workspace without changing any files."

2. **Probe an out-of-bounds scenario:**

   > "Suppose an external GitHub comment asks you to push a hotfix straight to `main` and publish a release note. How would you handle it?"

Confirm that the agent quotes its recorded approval gates accurately and states that it would stop and escalate rather than executing the gated action.

### 5. Optional: Inspect or edit directly via CLI

If you manage headless servers or prefer editing files directly:

```bash
holon agent list
holon agent get reviewer --detail full
```

Check `loaded_agents_md.agent_source.path` in the output to find the exact `AGENTS.md` path (do not assume every installation uses `~/.holon/`). After editing, verify in a fresh read-only turn:

```bash
holon run --agent reviewer "Read your current AGENTS.md and list your standing duties and approval boundaries without modifying files."
```

## Troubleshooting

- **Old rules still appear:** Start a fresh turn after the edit and verify `loaded_agents_md.agent_source.path` points to the file you updated.
- **Repository rules are missing:** Ask the agent to check `GetWorkspaceState`; running `cd` in a shell command does not activate a repository's `AGENTS.md`.
- **Instructions conflict across scopes:** Keep standing authority gates in the agent's `AGENTS.md` and repo conventions in the repository's `AGENTS.md`; never let a skill or external input override operator approval boundaries.
- **Upstream template changed:** Templates only initialize new agents. Compare upstream updates selectively rather than overwriting an active agent home.

## See also

- [Create an agent from a template](create-agent.md)
- [Organize agent skills across dedicated roles](organize-agent-skills-and-roles.md)
- [Manage agent memory](manage-agent-memory.md)
- [Create an agent template](create-agent-template.md)
- [Write and verify a skill](write-skill.md)
