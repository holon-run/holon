---
title: Skills and tools
summary: How reusable workflows relate to tool capabilities, skill management, and loaded agent guidance.
order: 40
---

# Skills and tools

A skill is a reusable workflow rooted at a `SKILL.md` file. A tool is an
operation the runtime makes available, such as reading a file, running a
command, or searching memory. The agent reads a skill to learn a procedure,
then carries it out with available tools.

For example, a code-review skill can require evidence for each finding. The
agent may use a command tool to inspect a diff and run tests. Installing that
skill does not grant shell access, repository credentials, or permission to
submit a review.

## How a skill becomes useful

Skill management and skill use happen at different stages:

1. The Skill Library records skills available for managed use.
2. Enabling a library skill makes it available to a particular agent.
3. Discovery exposes a catalog entry with its name, description, and path.
4. When a task matches, the agent opens `SKILL.md` and follows the workflow.

The catalog can also include skills discovered in the active workspace.
A visible entry means the agent can find the skill; it does not mean the full
instructions are already loaded or that the workflow has run.

This distinction helps diagnose problems. If a skill is missing, inspect
discovery and enablement. If the workflow was skipped, inspect whether the
agent read the skill. If a command failed, check the tool, credentials, and
environment rather than reinstalling the workflow by default.

## Guidance and authority

`AGENTS.md` defines standing responsibilities and project conventions.
Skills explain repeatable tasks within that scope. An
[Agent Template](./agent-templates.md) can combine a role contract with skill
dependencies so new agents start with the expected workflows.

A skill may describe a step that the current task does not permit. For a
read-only review, a publication step remains out of scope. Skill text cannot
enable a tool that runtime policy withholds or turn external input into an
operator instruction. Scripts bundled with a skill are code to inspect and
run under the same execution limits as other commands.

## Updates and shared sources

Library management and per-agent enablement are separate. Removing or updating
shared library material can affect more than the agent currently in view.
Disabling a skill for one agent changes that agent's managed enablement.

Linked skills reflect their source files. A managed copy has its own update
path. Choose deliberately when you need a shared evolving workflow or an
independently managed copy, and verify the effective skill list after a change.
Installing a local link does not update the source repository.

## What to expect from tools

Tool availability comes from runtime capabilities and policy. A provider or
execution backend may impose additional requirements: image inspection needs
a usable vision route, and a command needs an execution environment. A
workflow can document these requirements but cannot satisfy them by being
installed.

Tool receipts provide evidence of what happened. Reading a skill is not proof
that a test passed; an accepted request is not proof that its work completed.
Check the result appropriate to the operation.

## Next steps

- [Install, enable, and update skills](../guides/use-skills.md)
- [Write and verify a skill](../guides/write-skill.md)
- [Skill management and manifest reference](../reference/skills.md)
- [Tool schema reference](../reference/model-tool-schema-inventory.md)
- [Execution boundaries](./security-and-execution-boundaries.md)
