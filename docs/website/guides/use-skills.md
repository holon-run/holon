---
title: Discover, enable, and verify agent skills
summary: Extend an agent's capabilities with specialized skills through conversational prompts, verify tool boundaries in AGENTS.md, and run minimal test workflows without memorizing CLI syntax.
order: 40
---

# Discover, enable, and verify agent skills

As your AI coding agent tackles broader workflows—such as inspecting GitHub pull requests, processing Office documents (`docx`, `xlsx`), or performing security audits—you need a way to grant it domain expertise without bloating its default prompt context or compromising security.

In Holon, capabilities are modularized as **Skills**. A skill is an on-demand workflow rooted at a structured `SKILL.md` file. Holon manages skills through a two-tier model: skills are stored once in a shared **Skill Library** on the host, and enabled per agent as needed.

Crucially, you don't need to memorize CLI subcommands to expand your agent's abilities. You can instruct your agent in natural language to check its available skills, enable a new workflow, enforce trust boundaries in `AGENTS.md`, and verify tool execution.

This guide walks through discovering skills, enabling them through conversation, verifying safe execution with a minimal task, and disabling skills when work is done. For underlying mechanics, see [Skills and tools concept](../concepts/skills-and-tools.md) and [Skills reference](../reference/skills.md).

## Before you start

- A running Holon instance with the [Web GUI](./use-web-gui.md) open (default `http://localhost:8080`).
- An active agent (such as `developer` or `main`).
- A target task scenario requiring specialized capabilities (e.g. GitHub triage, document parsing, or code review).
- **Understanding security boundaries:** Installing a skill provides instructions and references to specialized tools; it does not bypass the agent's permission boundaries. High-risk operations (such as pushing code, publishing comments, or modifying production files) still require explicit authorization recorded in `AGENTS.md`.

## Steps

### 1. Check current capabilities and discover skills in conversation

Instead of running terminal queries, ask your agent directly in the Web GUI chat whether it has the skills required for your task:

> "Can you review pull request #42 and check for breaking API changes? Check whether you have the necessary GitHub review skills enabled."

**What happens behind the scenes:**
- The agent checks its active skills catalog.
- If the skill is not currently enabled, the agent inspects the shared Skill Library or template recommendations (e.g. `holon-run/holon/skills/ghx` or `code-review`).
- The agent responds explaining what it can do and what is missing:
  > *"I have general code inspection capabilities, but I do not currently have the `ghx` (GitHub CLI) or `code-review` skills enabled to interact with PR #42. Would you like me to enable the `ghx` skill for this agent?"*

### 2. Instruct the agent to enable the skill and record boundaries

Grant permission and specify the required authority limits in conversation:

> "Yes, please enable the `ghx` skill. Note that you may only read PR diffs and draft review comments; do not submit reviews or approve pull requests without my explicit confirmation."

**What happens behind the scenes:**
1. **Shared Library lookup:** The agent invokes Holon's managed skills interface to add the skill to the host's Skill Library (if not already cached) and enables it for its own profile.
2. **Permission enforcement:** The agent updates its persistent `AGENTS.md` instructions, recording the review-only constraint to ensure it does not overstep its authority during execution.
3. **Context isolation:** Holon registers the skill's metadata and makes its `SKILL.md` path discoverable in the agent's catalog, without loading the full text into memory until actually needed.

The agent confirms:
> *"The `ghx` skill is now enabled. I have recorded in my operating rules that all review submissions remain draft-only and require your explicit sign-off."*

### 3. Verify with a minimal test task

Before handing off critical or high-volume work, verify that the agent correctly reads the skill's instructions and operates within safe boundaries:

> "Fetch the metadata and diff for PR #42 and provide a 3-bullet summary in chat. Do not publish any comments on GitHub."

**Observe the execution in the Web GUI:**
1. **Skill loading:** The agent detects that this task matches the newly enabled `ghx` skill. It reads the local `SKILL.md` file first to confirm prerequisites and approved command patterns.
2. **Tool execution:** The agent runs read-only commands (e.g. `gh pr view 42`) within its execution boundary.
3. **Boundary adherence:** It outputs the 3-bullet summary in the chat, without making any external modifications or posting comments to GitHub.

### 4. Disable or remove skills when finished

When a specialized project concludes (for example, after finishing a one-time migration or document audit), you can disable the skill to keep the agent's focus clean:

> "We have completed the PR reviews. Please disable the `ghx` skill for this agent."

The agent disables the skill in its profile. The skill remains preserved in the shared host library for other agents to use, but will no longer be visible or callable by this agent.

---

## Optional: Advanced CLI management and template manifests

For automated CI/CD runners, container initialization, or headless server provisioning, Holon provides full CLI control over the Skill Library.

### Managing skills via the CLI

```bash
# View all skills installed in the shared library
holon skills catalog

# Add a skill from a local directory or remote Git repository
holon skills add /path/to/local/skill
holon skills add holon-run/holon/skills/ghx@main

# Enable a skill for a specific agent
holon skills enable ghx --agent developer

# List all active skills for an agent
holon skills list --agent developer

# Check for updates or lockfile differences
holon skills check ghx
holon skills update ghx

# Disable a skill for one agent
holon skills disable ghx --agent developer

# Remove a skill from the shared library entirely
holon skills remove ghx
```

### Declaring skills in agent templates

If you are building reusable agent templates (see [Agent templates reference](../reference/agent-templates.md)), declare pre-configured skills in `skills.toml`:

```toml
[skills]
required = [
  "holon-run/holon/skills/ghx",
  "holon-run/holon/skills/code-review"
]
```

When an agent is instantiated from the template, Holon automatically attaches the declared skills to its active catalog.

## Next steps

- [Skills and tools concept](../concepts/skills-and-tools.md): Learn how Holon handles tool dispatch, model contexts, and runtime trust.
- [Work in a project workspace or Git worktree](./work-with-workspaces.md): Configure isolated execution workspaces for your agent.
- [Observe, track, and verify agent progress](./observe-agent-work.md): Monitor live progress and verify artifact deliverables.
