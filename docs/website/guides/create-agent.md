---
title: Create an agent from a template
summary: Pick a template, create an agent, and confirm it can take a task.
order: 10
---

# Create an agent from a template

A template initializes a new agent's role and optional skills. This guide
creates a reviewer without changing any existing agent. For the lifecycle,
see [Agent templates](/concepts/agent-templates.md).

## Before you start

- The daemon is running: `holon daemon start`.
- A working model connection; see [Configure models](/guides/configure-models.md).
- An unused agent ID. Run `holon agent list` before choosing one.
- A template you have inspected. The
  [template reference](/reference/agent-templates.md) describes the roles;
  listing a role there does not mean it is installed on your machine.

## Steps

1. Open the Web GUI's **Agent Templates** page (`/templates`). Inspect the
   template's role contract, skill dependencies, and source. If `code-reviewer`
   is missing, use **Install Template** with:

   ```text
   https://github.com/holon-run/holon/tree/main/agent_templates/code-reviewer
   ```

   To maintain a whole source instead, add `https://github.com/holon-run/holon`
   under remote sources and use **Sync Sources**. Wait for the job to finish
   and refresh the catalog. Select the installed entry; name collisions can
   give it a different local install ID.

   There is no `holon templates` CLI command. For a CLI-only path, use an
   inspected local template directory or an explicit GitHub tree URL in
   `--template`, as shown below.

2. Create the agent, replacing `reviewer-demo` with your unused ID and
   `code-reviewer` with the installed selector:

   ```bash
   holon agent create reviewer-demo --template code-reviewer
   ```

   Without `--template`, Holon uses its hidden generic default. Template
   initialization refuses to overwrite a non-empty agent home.

3. Check the agent and its skills:

   ```bash
   holon agent list
   holon agent get reviewer-demo --detail full
   holon agent status reviewer-demo
   holon skills list --agent reviewer-demo
   ```

   Use `loaded_agents_md.agent_source.path` in the full detail to locate
   `AGENTS.md`. Inspect it and, when present, `template-provenance.json` in
   the same home. Confirm that they identify the role and source you chose.

4. Give it a small, read-only task:

   ```bash
   holon run --agent reviewer-demo "Read your role contract and list your review responsibilities and approval boundaries. Do not access GitHub or change files."
   ```

   This checks initialization without needing access to a repository or PR.

## Confirm it worked

The agent appears in the list, its home contains the selected contract, and
its effective skill list matches the template's dependencies. The test returns
a brief consistent with the role. An idle agent need not remain awake.

## Troubleshooting

- **Template not found:** check the installed selector in the GUI, or use an
  absolute path or GitHub tree URL. A role name alone does not install it.
- **Home already exists:** choose a new ID. Do not delete the existing home or
  reapply a template to customize it; use
  [Customize an agent](/guides/customize-agent.md).
- **Skill bootstrap failed:** inspect the create result and missing source.
  After fixing access or dependencies, run `holon agent repair reviewer-demo`
  and check `holon skills list --agent reviewer-demo` again.
- **Task cannot run:** inspect `holon config doctor` for model availability.

<a id="when-the-built-in-catalog-is-not-enough"></a>

## Use another template source

Choose one of these alternatives, using an unused agent ID:

```bash
holon agent create my-agent --template /path/to/my-template
holon agent create my-agent --template https://github.com/owner/repo/tree/main/templates/my-template
```

Template layout, selection rules, and the `template.toml` and `skills.toml`
schema are in the [Agent templates reference](/reference/agent-templates.md).
To author one, follow [Create an agent template](/guides/create-agent-template.md).
