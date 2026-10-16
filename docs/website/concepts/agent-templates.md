---
title: Agent templates
summary: Reusable starting points for an agent's role and skills, separate from the agent instance they create.
order: 30
---

# Agent templates

An Agent Template supplies the starting role contract for a new agent and,
optionally, skills to install for it. Use one when you want several agents to
start with a consistent responsibility, such as code review. For a small
one-off question, the default agent may be enough.

## From template to agent

The template library makes reusable templates available for selection.
Installing or syncing a template puts it in that library; it does not create
an agent. Creating an agent with a selected template initializes a separate
Agent Home.

For example, `code-reviewer` can be the template while `payments-reviewer` is
the agent ID. The template name selects the starting material. The agent ID
addresses the worker that now owns its own guidance, memory, and work.

When no template is selected, Holon uses its built-in default bootstrap.
Named catalog templates must be available before selection; the built-in
default does not mean the full template catalog is already installed.

## Role, workflow, and execution

A reviewer illustrates the relationship between four objects:

| Object | Contribution to a review |
|---|---|
| Template | Starts the agent with a review role and optional skill dependencies. |
| Agent | Owns the review conversation, work records, and eventual report. |
| Skill | Describes how to inspect changes and report supported findings. |
| Tool | Reads a diff, runs a check, or sends an authorized message. |

The template's `AGENTS.md` becomes the agent's role guidance. An optional skill
manifest names dependencies to install during initialization. Those dependencies
may need network access or local source files. A successful template selection
does not supply provider credentials, access to a private repository, or
permission to publish a review.

In particular, a reviewer template can explain an approval workflow without
authorizing approval for every task. The current request and runtime policy
still govern what the agent may do.

## Updating templates and existing agents

Treat the role contract as a creation-time starting point. Updating or syncing
the library template does not rewrite the role files of existing agents.
Renaming a template does not rename their agent IDs. Holon refuses template
initialization over a non-empty Agent Home to avoid overwriting existing state.

To revise `payments-reviewer`, edit its role guidance and manage its enabled
skills explicitly. To test a revised template, create a fresh agent and check
the resulting guidance and skill availability.

Skills have their own update and enablement lifecycle. A linked skill can
reflect changes to its source even though the agent's copied role contract has
not changed. Do not infer that all template-related content is permanently
frozen, or that all of it follows library updates. See
[Skills and tools](./skills-and-tools.md).

## Choosing a template

Choose by responsibility and boundaries, then inspect the role contract.
A maintenance template that waits for future events has different expectations
from a one-shot solver. Neither a descriptive name nor a preinstalled skill
substitutes for reviewing those expectations.

The [template reference](../reference/agent-templates.md) lists the catalog,
selection rules, directory layout, and manifest formats. You do not need to
edit a manifest just to create an agent from an existing template.

## Next steps

- [Create an agent from a template](../guides/create-agent.md)
- [Write and reuse a template](../guides/create-agent-template.md)
- [Customize an existing agent](../guides/customize-agent.md)
- [Agents, roles, and guidance](./agents.md)
