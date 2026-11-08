---
title: Organize agent skills across dedicated roles
summary: Avoid tool clutter and instruction conflicts by giving each long-lived agent its own focused skills, boundaries, and workspace permissions.
order: 89
---

# Organize agent skills across dedicated roles

This guide shows how to divide an expanding set of skills across focused, long-lived agents. You will learn how to inspect and enable dedicated skills in conversation via the Web GUI, establish clear authority boundaries, and coordinate specialized agents through work items and delegation—eliminating prompt bloat, tool confusion, and instruction collisions.

- To understand why overloading one agent with dozens of skills degrades model reasoning and compromises safety, read the blog post: [Why your agent gets dumber with more skills (and how to fix it)](../blog/why-agents-need-skill-isolation.md).
- For manual CLI library commands and skill packaging, see [Install, enable, and update skills](./use-skills.md).

## Before you start

- Holon is running with access to the [Web GUI](./use-web-gui.md) (default `http://localhost:8080`) and at least one [configured model](./configure-models.md).
- The shared skill library contains relevant skills (official templates bundle standard skills like `code-review`, `ghx`, `github-review`, and `sview`).
- No need to memorize complex CLI flags: skill inspection, scoping, and boundary checks can be handled directly in conversation with the agent.

## Steps

### 1. Identify distinct roles and avoid the "God Agent"

Before spinning up an agent, break down recurring workflows into 2 or 3 specialized roles. In a typical engineering team, these roles have very different goals:

| Role Name | Mandate | Recommended Skills | Authority Boundary |
| --- | --- | --- | --- |
| **Developer (`developer`)** | Implement features, fix bugs, run builds | `ghx`, `sview`, file edit and test tools | Write access to active worktree; cannot merge unreviewed PRs |
| **Reviewer (`reviewer`)** | Scrutinize changes, detect risks, review PRs | `code-review`, `github-review`, `ghx` | **Strictly read-only**; never mutates application code |
| **Docs Steward (`docs-steward`)** | Maintain documentation, check links, reduce ambiguity | `interlinked-markdown-wiki`, doc check tools | Confined to docs and assets; no backend write permissions |

In the Holon Web GUI, open **Templates** from the left navigation and create agents from official templates (such as `software-developer`, `code-reviewer`, or `docs-steward`).

### 2. Instruct the agent to inspect and trim its active skills

Open the chat view for a specific agent (such as the newly created `reviewer`). Instruct it in plain language to audit its own skill profile:

```text
Check which skills are currently enabled for you. As a code reviewer, confirm whether you have the tools needed for read-only review, and list any unnecessary write or external posting skills that should be disabled.
```

The agent reads its configuration and active tools, returning a breakdown:
- **Core review tools present**: `code-review` (structured analysis), `ghx` (PR inspection).
- **Suggested removals**: If unnecessary file mutation or external posting tools are attached, the agent flags them for removal.

To enable a specific skill from the shared library, simply ask in conversation:

```text
Enable the github-review skill for yourself to support line-by-line PR reviews, but do not enable any tool that mutates files.
```

The agent enables the requested skill and confirms its updated tool inventory.

### 3. Record boundaries permanently in `AGENTS.md`

To ensure operational discipline survives across future turns, instruct the agent to persist its standing rules in its guidance file:

```text
Please record these operational boundaries in your AGENTS.md:
1. Your sole responsibility is code quality review with primary evidence; never call file-mutation tools on reviewed code.
2. Keep each review scoped strictly to the changes in the current PR.
3. Mark severe defects as blocking and wait for the human author to fix them; do not attempt unprompted fixes.
```

The agent updates its `agent_home/AGENTS.md`. These boundaries remain active in every subsequent session.

### 4. Provide isolated workspace roots

Pair skill boundaries with physical environment isolation:

1. **Dedicated branch worktrees for developers**:
   Instruct the developer agent:
   ```text
   Attach the current repository and start work on issue-102 inside an isolated Git worktree.
   ```
   The agent creates and activates an isolated worktree branch, ensuring edits and test artifacts do not pollute other branches.

2. **Clean read-only projections for reviewers**:
   A reviewer agent inspects canonical checkouts or remote PR commits without holding write locks on active development trees.

### 5. Coordinate across agents using work items and delegation

When a developer finishes an implementation, do not force the developer agent to review its own work. Instead, delegate:

In the developer agent's chat, instruct:
```text
Implementation and local tests are complete. Create a work item delegating this Git commit to the reviewer agent for review, and wait for its findings.
```

1. The developer agent creates a clear WorkItem and issues an asynchronous delegation.
2. The reviewer agent activates in its clean, read-only context, evaluates the patch using `code-review`, and logs structured review evidence.
3. Once review finishes, an event wakes the developer agent to address findings or conclude the task.

## Verification and self-checks

Confirm your role and skill isolation with these verification steps:

### 1. Check prompt overhead
Send a brief message (such as "State your core duties") to the agent in the Web GUI. Verify response latency and token count: a focused role should maintain 3 to 6 active tools, avoiding massive initialization prompts.

### 2. Test boundary enforcement
Send an intentional out-of-scope command to the reviewer agent:
```text
Change line 20 of src/main.rs to print "hello" and commit the change.
```
**Expected outcome**: The reviewer agent explicitly refuses, citing its `AGENTS.md` contract and read-only review mandate, and suggests handing the task to the developer agent.

### 3. Verify via CLI (optional)
If you prefer the terminal, verify library registration and per-agent enablement:

```bash
# Inspect all skills in the shared library
holon skills catalog

# Inspect effective skills for a specific agent
holon skills list --agent reviewer
```

Verify that only role-appropriate skills appear in the output.

## Frequently asked questions

### Q: What if a task requires both coding and documentation updates?
A: Keep the primary feature work item with the developer agent. When code changes land, delegate doc updates to `docs-steward` under the same workflow. Specialization avoids creating a fragile "all-in-one" persona.

### Q: Why not enable all skills globally?
A: Global injection forces every agent to load every tool schema into its prompt. This causes attention dilution, parameter hallucinations between similar tools, and unnecessary token costs on every turn.

### Q: Can I apply this role isolation pattern in other tools like Claude Code or Codex?
A: Absolutely. With CLI agents, you can establish dedicated role directories (such as `~/.agents/code-reviewer`), place scoped `CLAUDE.md` / `AGENTS.md` and local `.mcp.json` configs inside them, and point the agent to target projects at runtime. For a step-by-step breakdown and architectural evolution, see the blog post [Why your agent gets dumber with more skills](../blog/why-agents-need-skill-isolation.md).

## Next steps

- Set up an autonomous PR reviewer with event triggers: [Build a local Code Reviewer that follows PRs](./build-code-reviewer.md).
- Author custom skills tailored to your private workflows: [Write and verify a skill](./write-skill.md).
- Manage multi-repo projects and isolated Git worktrees: [Work in a project workspace or worktree](./work-with-workspaces.md).

