---
title: Install, enable, and update skills
summary: Manage the shared Skill Library and verify the skills available to a selected agent.
order: 40
---

# Add a skill to an agent

A skill is a `SKILL.md` workflow an agent can load on demand. Skills live in a
shared library: you install to the library once, then enable per agent.
For loading and authority boundaries, see
[Skills and tools](/concepts/skills-and-tools.md).

## Before you start

- The daemon is running.
- An existing target agent; replace `reviewer` below with its ID.
- A trusted skill source whose `SKILL.md` you have reviewed. Installation does
  not grant tool permissions or install every external program it mentions.

## Steps

1. See what is already in the library:

   ```bash
   holon skills catalog
   ```

2. Add a skill using one source form:

   ```bash
   holon skills add /path/to/skill-dir
   holon skills add https://github.com/user/repo/tree/main/skills/my-skill --remote
   ```

   Replace the examples with your source. Local skills are linked by default;
   use `--copy` only when you need an independent managed copy. For GitHub,
   `owner/repo/path@ref` is also accepted, for example:

   ```bash
   holon skills add holon-run/holon/skills/ghx@main
   ```

   Use the skill name reported by the catalog in the remaining commands.

3. Enable it for one agent:

   ```bash
   holon skills enable my-skill --agent reviewer
   ```

4. Confirm the agent sees it:

   ```bash
   holon skills list --agent reviewer
   ```

5. Give the agent a small task that matches the skill, explicitly asking it
   to read `SKILL.md` first. Keep the test within the skill's prerequisites
   and your authorized scope. Inspect the turn:

   ```bash
   holon transcript --agent reviewer --limit 30
   ```

## Confirm it worked

`holon skills list --agent reviewer` shows the intended source and scope.
The test transcript shows the agent read that `SKILL.md` before using its
workflow. Catalog visibility alone does not prove loading or execution.

## Keeping skills current

```bash
holon skills update my-skill
holon skills check my-skill
holon skills list --agent reviewer
```

Updates use recorded remote-source metadata. Linked local sources reflect
edits to their source directory; `update` does not update that source repository.
Omit the name to update or check all entries. These are shared-library
operations, so consider other agents before updating a dependency.

## Disable or remove a skill

```bash
holon skills disable my-skill --agent reviewer
holon skills list --agent reviewer
```

Disable the agent-level entry when only this agent should stop using it.
`holon skills remove my-skill` removes the library entry and can affect other
agents; check users of the shared skill before doing so. A same-named skill
from another discovery scope may still appear in the effective list.

## Troubleshooting

- **Name not found:** inspect `holon skills catalog`; use its name or source
  selector rather than assuming the directory name was retained.
- **Changed files are not discovered:** run `holon skills refresh`, then
  inspect the effective list and its source path.
- **Lock mismatch:** run `holon skills check my-skill`, inspect the reported
  difference, and use `holon skills reconcile my-skill` only when the local
  contents are the version you intend to record.
- **Available but not used:** choose a matching task and ask the agent to read
  the skill. Check missing tools or dependencies separately.

Command names, source rules, and the `skills.toml` schema are in the
[Skills reference](/reference/skills.md) and
[template manifest reference](/reference/agent-templates.md#skillstoml).
