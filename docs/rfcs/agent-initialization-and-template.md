---
title: RFC: Agent Initialization and Template
date: 2026-04-22
status: accepted
issue:
  - 367
---

# RFC: Agent Initialization and Template

## Summary

Holon should define agent initialization as a first-class runtime contract.

The core model is:

- every agent has its own `agent_home`
- `agent_home` is the agent's identity root, not just a storage directory
- every agent may have its own agent-scoped `AGENTS.md`
- every agent may have agent-local skills
- agent creation should prefer a reusable template over passing large inline
  instruction blobs
- a template initializes an agent, but does not remain the live source of truth

After initialization, the effective source of truth for the agent's role and
long-lived behavioral guidance is the agent's own `agent_home/AGENTS.md`, which
may evolve over time.

## Why

Holon already defines:

- where agent-scoped and workspace-scoped instructions load from
- where user, agent, and workspace skill catalogs are discovered

But it does not yet define the missing step in between:

- when a new agent is created, what initial identity materials does it get
- where role-specific guidance should live
- where agent-specific skills should live
- how a long-lived named agent differs from a temporary child agent at
  initialization time

Without this contract, several needs are awkward or unstable:

- role-specific agents such as reviewer, developer, or release assistants
- agent-specific behavioral constraints that do not belong in workspace
  instructions
- agent-specific skills that should not be global or project-wide
- spawn flows that would otherwise need to pass large instruction payloads

## Scope

This RFC defines:

- the initialization contract for any new agent
- the role of `agent_home` in that contract
- how agent-scoped `AGENTS.md` should be initialized
- how agent-local skills should be initialized
- the role of templates in agent creation
- how initialized guidance may evolve after agent creation

This RFC does not define:

- runtime-enforced permission or policy fields
- a full structured authorization system
- the exact operator UX for editing templates or `AGENTS.md`
- hierarchical `AGENTS.md` loading inside the workspace tree

## Core Model

### `agent_home` is the agent identity root

Every agent has an `agent_home`.

`agent_home` should be treated as the agent's local identity root. It may hold:

- the agent's `AGENTS.md`
- agent-local skill links or entries
- agent-local supporting files that belong to that agent's role or workflow
- durable agent state already managed by the runtime

This applies uniformly to:

- the default agent
- named agents
- child agents

The difference between these agents is lifecycle and retention policy, not
whether they are allowed to have an `agent_home`.

### Agent-scoped `AGENTS.md`

Each agent may have its own `agent_home/AGENTS.md`.

This file is the agent-local durable guidance document for:

- role definition
- responsibilities
- textual permission boundaries
- long-lived collaboration expectations
- agent-specific workflow preferences
- agent-specific skill usage expectations

This file is not a runtime-enforced policy object. Its effect is prompt-level
guidance, not hard execution enforcement.

### Agent-local skills

Each agent may also have agent-local skills rooted under its own `agent_home`.

These skills exist so that:

- a skill can belong to one agent without becoming global
- a skill can belong to one agent without becoming workspace-wide
- a template can attach role-specific workflows to an agent identity

This RFC does not require that agent-local skills be copied into `agent_home`.
They may be represented by linked or referenced local skill entries, as long as
they appear as agent-scoped skills to the runtime.

## Initialization Contract

When Holon creates a new agent, initialization should establish a local
identity root for that agent rather than just a blank state directory.

The initialization contract should define:

- the agent identity record
- the agent's `agent_home`
- the initial agent-scoped `AGENTS.md`, if any
- the initial agent-local skill attachments, if any
- any explicit bootstrap metadata needed to explain how that state was created

Initialization should not require sending a large inline instruction blob
through normal spawn prompts.

## Template Model

### Templates are initializers

Holon should support an agent template mechanism for initializing new agents.

The purpose of a template is to provide reusable bootstrap content such as:

- an initial `AGENTS.md` body or rendered `AGENTS.md` template
- initial agent-local skill references

The template should be used to materialize the initial agent identity state in
`agent_home`.

### Phase-1 template format

The phase-1 template format should stay minimal and local-first.

A template should be represented as a local directory. The directory name is the
template id.

The minimal phase-1 shape is:

- `AGENTS.md`
  - required
  - used as the initial agent-scoped `AGENTS.md`
- `skills.json`
  - optional
  - used to attach initial agent-local skill references

Phase 1 should not require a separate `template.json` metadata file. If an
operator needs a stable template identifier, the directory name should be used.

### Template selector

Phase 1 should use a single `template` selector rather than separate template id
and template path fields.

The selector should support exactly three forms:

- `template_id`
- absolute local path
- GitHub URL

Resolution should work like this:

- if `template` is an absolute path, use that local template directory
- if `template` is a GitHub URL, treat that URL as the template source
- otherwise, treat it as `template_id` and resolve it from
  `~/.agents/templates/<template_id>/`

Phase 1 should not add multi-root template discovery.

`template_id` should be a simple stable name, not a path-like string.

### GitHub URL format

Phase 1 should keep GitHub template URLs explicit and narrow.

The accepted GitHub URL form should be:

```text
https://github.com/<owner>/<repo>/tree/<ref>/<path-to-template-dir>
```

The URL must point to the template directory itself, not just a repository root
or a higher-level folder.

The target directory should contain:

- `AGENTS.md`
- optional `skills.json`

Phase 1 should not require support for:

- repository root URLs
- URLs that rely on implicit default template locations inside a repository
- raw-content URLs
- multiple GitHub URL variants with different semantics

If the URL does not resolve to a readable directory with the expected template
shape, template application should fail.

### Builtin templates

Holon should ship a small builtin template set for common roles.

Phase 1 should seed builtin templates into `~/.agents/templates/` at startup so
they become normal user-visible templates.

This seeding should be idempotent:

- builtin templates are installed only when the target template directory does
  not already exist
- startup should not overwrite or silently rewrite an existing user template
- after seeding, normal `template_id` resolution uses the user template
  directory

This keeps builtin templates simple and inspectable without introducing a
separate builtin template discovery layer.

### `skills.json` format

The phase-1 `skills.json` format should stay minimal.

It should be a JSON object with one field:

```json
{
  "skill_refs": [
    {
      "kind": "local",
      "path": "/absolute/path/to/local-skill"
    },
    {
      "kind": "github",
      "package": "vercel-labs/agent-skills@react-best-practices"
    }
  ]
}
```

The rules are:

- `skill_refs` is an array of typed skill references
- local references use:
  - `kind = "local"`
  - `path = "/absolute/path/to/skill"`
- local paths must be absolute paths
- relative local paths are not allowed
- a local path points to a skill directory whose entrypoint is `SKILL.md`
- GitHub references use:
  - `kind = "github"`
  - `package = "<owner>/<repo>@<skill>"`
- the GitHub package string should follow the same package-style convention used
  by `npx skills add`
- this RFC does not require the manifest to expose lower-level installer fields
  such as separate repo and path keys
- invalid, unreadable, or unresolvable entries should cause template
  application to fail rather than silently produce a partially initialized
  agent

Phase 1 does not require additional per-skill metadata in the manifest. The
runtime may derive name and description from the referenced skill itself.

### Materializing agent-local skills

Phase 1 should materialize template-attached skills into normal agent-scoped
skill roots under `agent_home`.

The goal is for runtime skill discovery to continue working against ordinary
agent-local skill directories.

The intended phase-1 behavior is:

- local skill refs may be materialized as symlinks into the agent skill root
- GitHub skill refs may be materialized as installed skill directories in the
  agent skill root
- failed skill resolution or installation should fail template application

Phase 1 should not require a separate runtime skill manifest format for already
initialized agents.

### Templates are not the live source of truth

After initialization, the template should not remain the live source of truth
for the agent.

Instead:

- the live source of truth is the agent's own `agent_home/AGENTS.md`
- the runtime may record template provenance such as template id or source
- later template changes should not silently rewrite an already-created agent

If an operator wants to realign an existing agent with a template, that should
be an explicit action, not an automatic background update.

## Evolving `AGENTS.md`

### `AGENTS.md` is allowed to evolve

Agent-scoped `AGENTS.md` should be allowed to change after initialization.

This is important because the file acts as durable agent-local memory for:

- role refinement
- clarified responsibilities
- updated expectations from operator feedback
- accumulated long-lived guidance that does not belong in one task prompt

The operator may direct the agent to update this file as part of normal
collaboration.

### Effective timing

Updates to `agent_home/AGENTS.md` should take effect on the next prompt
assembly. They do not need to interrupt an in-flight turn.

This keeps behavior inspectable and avoids mid-turn instruction mutation.

### Content boundary

Agent-scoped `AGENTS.md` should primarily capture long-lived role and
collaboration guidance.

It should not become the default place for:

- one-off task goals
- temporary acceptance criteria for a single issue
- transient debugging notes
- short-lived operator prompts that belong to the current turn only

Those belong in operator prompts, work items, or other task-scoped runtime
structures.

## Uniform Agent Treatment

Holon should treat all agents uniformly for initialization:

- every agent may have an `agent_home`
- every agent may have agent-scoped `AGENTS.md`
- every agent may have agent-local skills

The distinction between default, named, and child agents should come from
lifecycle and retention policy rather than a separate initialization model.

For example:

- long-lived agents normally retain their `agent_home`
- a temporary child agent may have its `agent_home` cleaned up when the agent is
  retired and removed

This cleanup policy does not change the initialization contract while the agent
exists.

## Relationship To Existing RFCs

This RFC builds on, and does not replace:

- [Instruction Loading](./instruction-loading.md)
- [Skill Discovery and Activation](./skill-discovery-and-activation.md)
- [Workspace Binding and Execution Roots](./workspace-binding-and-execution-roots.md)

Those RFCs define where instructions and skills are loaded from. This RFC
defines how a new agent gets its own local instruction and skill roots in the
first place.

## Initial Direction

The intended phase-1 direction is:

1. new agents can be initialized from a template
2. template selection uses one `template` selector that accepts `template_id`,
   absolute path, or GitHub URL
3. `template_id` resolution uses `~/.agents/templates/<template_id>/`
4. Holon seeds a small builtin template set into `~/.agents/templates/` on
   startup without overwriting existing user templates
5. templates materialize an initial `agent_home/AGENTS.md`
6. templates may attach agent-local skill references
7. phase-1 templates are local directories and do not require a separate
   metadata file
8. initialized agent-local skills are materialized into normal agent-scoped
   skill directories under `agent_home`
9. the runtime records template provenance, but not a persistent parallel copy
   of the expanded instruction text as the live identity source
10. phase 1 does not provide a dedicated refresh-from-template action
11. later edits to `agent_home/AGENTS.md` are allowed and become effective on the
   next turn
