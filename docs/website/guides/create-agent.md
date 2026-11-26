---
title: Create an agent from a template
summary: Instantiate a dedicated role agent in the Web GUI from an inspected template, confirm its isolated home and skills, and verify it with one read-only task.
order: 10
---

# Create an agent from a template

Use this guide to create a new dedicated agent from an inspected template in the
Web GUI and confirm it can accept work without altering existing agents.

- A template initializes a new agent's `AGENTS.md` role contract and declared
  skills inside its own `agent_home`; it does not grant extra host permissions
  or automatically trust external content. See [Agent templates](/concepts/agent-templates.md).
- Use this guide when you need a **separate agent identity**. To adjust standing
  responsibilities or approval gates on an existing agent, follow
  [Create and customize a dedicated role agent with permission boundaries](customize-agent.md).

## Prerequisites

- A running Holon runtime with the [Web GUI](use-web-gui.md) open (default
  `http://127.0.0.1:7878/`) and a working model route
  ([Configure models](configure-models.md)).
- An inspected template from the [template reference](/reference/agent-templates.md)
  (listing a role in docs does not mean it is already installed locally).
- An unused descriptive agent ID (for example `reviewer-demo`).

## Steps

### 1. Inspect or install the template in the Web GUI

Open **Agent Templates** (`/templates`) in the Web GUI to review a template's
role contract, skill dependencies, and source before instantiating it.

- If `code-reviewer` is already listed, click it to check its manifest and
  bundled skills.
- If it is missing, click **Install Template** and enter:

  ```text
  https://github.com/holon-run/holon/tree/main/agent_templates/code-reviewer
  ```

  To keep the full official catalog available locally, add
  `https://github.com/holon-run/holon` under remote template sources and click
  **Sync Sources**.

You can also ask your primary agent (`main`) in chat to check which templates
are currently installed and which unused IDs are available before creating one.

### 2. Create the new agent instance

Choose one of these equivalent Web GUI paths:

- **From `/templates`**: Select the installed `code-reviewer` entry, click
  **Create Agent**, and enter an unused ID such as `reviewer-demo`. Note that
  if two sources define the same template name, the local install selector may
  include a suffix.
- **In conversation with your primary agent**: Ask `main` to create the new
  instance:

  > "Create a new agent named `reviewer-demo` from the `code-reviewer` template. Do not modify any existing agents or attach repositories yet."

Holon creates a separate `agent_home` for `reviewer-demo` with its own
`AGENTS.md`, memory files, and template-declared skills. Template initialization
refuses to overwrite a non-empty home.

### 3. Run a minimal read-only verification task

Open `reviewer-demo` from the Web GUI Dashboard roster and send one read-only
check in its conversation bar:

> "Read your role contract and list your standing responsibilities, active skills, and approval boundaries. Do not access GitHub or modify any files."

Confirm that:

1. The agent appears as a distinct entry in the Dashboard roster.
2. Its reply matches the `code-reviewer` contract and expected skills without
   needing repository access or credentials yet.

An idle agent does not need to stay awake; once verified, you can attach a
repository and customize approval gates following
[Create and customize a dedicated role agent with permission boundaries](customize-agent.md).

## Troubleshooting

- **Template not found:** Confirm the installed selector on `/templates` (name
  collisions can assign a distinct local install ID) or install the exact
  GitHub tree URL first.
- **Agent ID already exists:** Choose a fresh unused ID. Never delete an active
  agent home or reapply a template just to change rules; use
  [Customize an agent](customize-agent.md).
- **Skill bootstrap failed:** Inspect the creation error on `/templates` or the
  Dashboard. After fixing network or source access, run
  `holon agent repair reviewer-demo` and re-check active skills.
- **Task fails to start:** Verify model availability on the runtime or run
  `holon config doctor`.

## Optional: CLI creation and custom template sources

On headless hosts or in setup scripts, inspect, create, and verify from the
terminal:

```bash
holon agent list
holon agent create reviewer-demo --template code-reviewer
holon agent get reviewer-demo --detail full
holon skills list --agent reviewer-demo
holon run --agent reviewer-demo "Read your role contract and list your review responsibilities and approval boundaries. Do not access GitHub or change files."
```

Check `loaded_agents_md.agent_source.path` in `holon agent get ... --detail full`
and `template-provenance.json` inside that home when auditing which template
source initialized the agent.

<a id="when-the-built-in-catalog-is-not-enough"></a>

### Use a local directory or custom GitHub template URL

Without `--template`, `holon agent create` creates a blank agent with a minimal
role-neutral `AGENTS.md` rather than assigning a role template. To instantiate
an inspected custom template directly:

```bash
holon agent create my-agent --template /path/to/my-template
holon agent create my-agent --template https://github.com/owner/repo/tree/main/templates/my-template
```

Template layout, selection rules, and the `template.toml` and `skills.toml`
schema are in the [Agent templates reference](/reference/agent-templates.md).
To author your own template, follow
[Create an agent template](/guides/create-agent-template.md).

## See also

- [Create and customize a dedicated role agent with permission boundaries](customize-agent.md)
- [Organize agent skills across dedicated roles](organize-agent-skills-and-roles.md)
- [Install, enable, and update skills](use-skills.md)
- [Create an agent template](create-agent-template.md)
