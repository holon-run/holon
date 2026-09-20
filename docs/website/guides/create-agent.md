---
title: Create an agent from a template
summary: Pick a template, create an agent, and confirm it can take a task.
order: 17
---

# Create an agent from a template

A template gives a new agent its starting role and, when it needs them, its
skills. Create the agent once, then address it by ID.

## Before you start

- The daemon is running: `holon daemon start`.
- You know the role you want. The catalog is in the
  [Agent templates reference](/reference/agent-templates.md).

## Steps

1. Create the agent from a template:

   ```bash
   holon agent create reviewer --template code-reviewer
   ```

   Without `--template`, the agent starts with a generic default contract. Use a
   template whenever the agent has a specific job.

2. Check that it exists and is ready:

   ```bash
   holon agent list
   holon agent status reviewer
   ```

3. Give it one task and watch the result:

   ```bash
   holon run --agent reviewer "Review the open PR on holon-run/holon#1234"
   ```

   For work that should outlive the command, start it from the TUI instead:
   `holon tui`.

## Confirm it worked

The agent appears in `holon agent list`, `holon agent status` shows it awake, and
your task ends with a result brief or a question for you.

## When the built-in catalog is not enough

Create from a template you control:

```bash
holon agent create my-agent --template /path/to/my-template
holon agent create my-agent --template https://github.com/owner/repo/tree/main/templates/my-template
```

Template layout, selection rules, and the `template.toml` and `skills.toml`
schema are in the [Agent templates reference](/reference/agent-templates.md).
