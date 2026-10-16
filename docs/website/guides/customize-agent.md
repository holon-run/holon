---
title: Customize an agent's role and guidance
summary: Update an existing agent's standing instructions without replacing its home or reapplying a template.
order: 20
---

# Customize an agent's role and guidance

Use an agent's `AGENTS.md` for its standing responsibilities and authority.
Use project guidance for rules that should follow the repository. See
[Agents and guidance](/concepts/agents.md) for how these scopes differ.

## Before you start

- Choose an existing agent you are authorized to customize.
- Wait for its active work to finish before changing instructions.
- Keep a backup or versioned copy of the file you will edit.

## Steps

1. Locate the agent home and check its state:

   ```bash
   holon agent list
   holon agent get reviewer --detail full
   holon agent status reviewer
   ```

   Replace `reviewer` throughout with your agent ID. The
   `loaded_agents_md.agent_source.path` field identifies the agent's guidance
   file. Use that path; do not assume a custom installation uses `~/.holon`.

2. Read the existing `AGENTS.md` before editing. Preserve useful instructions.
   Add a focused responsibility or boundary, for example:

   ```markdown
   ## Review reporting

   Report findings with a file location, impact, and verification evidence.
   If no findings are supported, say what you checked and what remains untested.
   Do not submit reviews, approve changes, or merge without operator approval.
   ```

   Edit that file with your editor. Do not replace the entire agent home or
   its runtime-managed `.holon/` state.

3. Put repository-specific rules in the repository's `AGENTS.md` instead,
   if you have permission to edit it. Keep current task progress in a WorkItem
   and reusable procedures in a [skill](/guides/write-skill.md).
   Avoid copying the same instruction into all three places.

4. Start a new, read-only test turn:

   ```bash
   holon run --agent reviewer "Read your current AGENTS.md. Describe how you would report a review with no supported findings. Do not access external services or change files."
   holon transcript --agent reviewer --limit 20
   ```

## Confirm it worked

The agent describes the new reporting rule and retains its existing authority
boundaries. Check the transcript for the file it read. A paraphrase alone does
not establish that every future task will follow the instruction.

## Troubleshooting

- **Old guidance appears:** check the home path and that the test started after
  your edit. Ask the agent to read the exact file again.
- **Project rules are missing:** inspect the active workspace with
  `GetWorkspaceState`; a shell `cd` does not activate project guidance.
- **Instructions conflict:** resolve the scope in the relevant `AGENTS.md`.
  Do not use a skill or lower-trust task text to override authority boundaries.
- **A template changed upstream:** compare the new template with your contract
  and apply only intended edits. Template sync does not update existing agents.

For a reusable starting point for new agents, see
[Create an agent template](/guides/create-agent-template.md).
