---
title: Add a skill to an agent
summary: Find a skill, install it to the library, enable it for an agent, and confirm it is active.
order: 18
---

# Add a skill to an agent

A skill is a `SKILL.md` workflow an agent can load on demand. Skills live in a
shared library: you install to the library once, then enable per agent.

## Before you start

- The daemon is running.

## Steps

1. See what is already in the library:

   ```bash
   holon skills catalog
   ```

2. Add a skill:

   ```bash
   holon skills add /path/to/skill-dir
   holon skills add https://github.com/user/repo/tree/main/skills/my-skill --remote
   ```

3. Enable it for one agent:

   ```bash
   holon skills enable my-skill --agent reviewer
   ```

4. Confirm the agent sees it:

   ```bash
   holon skills list --agent reviewer
   ```

## Confirm it worked

`holon skills list --agent reviewer` shows the skill, and the agent can now load
it when a task matches.

## Keeping skills current

```bash
holon skills update
holon skills check
```

Command names, source rules, and the `skills.toml` schema are in the
[Skills reference](/reference/skills.md).
