# Skill Discovery And Activation

This RFC defines the first skill model for `Holon`.

The immediate goal is to support:

- local skill catalogs
- policy-derived agent-scoped skill availability
- default-agent access to user-level skill libraries
- workspace-local skills
- file-based skill usage without overfitting to a dedicated load tool
- activation records that survive compaction and resume

This document intentionally treats a skill as a local artifact rooted at
`SKILL.md`.

It does not try to turn skills into a separate runtime subsystem before the
basic local contract is stable.

## Short Answer

`Holon` should treat a skill as a local instruction package rooted at
`SKILL.md`.

The first runtime model should distinguish:

- `catalog discovery`: which skill directories are scanned and listed
- `agent attachment`: which discovered skills are available to one agent
- `activation`: which attached skills are actually in use for the current work

The main rules are:

- skill bodies stay on disk
- catalog loading does not mean prompt injection
- `default agent` may discover user-level skills by default
- named and child agents should not discover user-level skills by default
- agents may use a skill by reading its `SKILL.md`
- runtime should still record observable activation state for inspectability,
  compaction, and resume

## Why This RFC Exists

Several current decisions now depend on a stable answer to "what does it mean
for a skill to be available or active?":

- `default agent` versus named-agent behavior
- `workspace_anchor` versus `agent_home` instruction roots
- future `create agent --skill ...` behavior
- prompt compaction and continuity
- whether `Holon` needs a dedicated skill-loading tool

Without a skill model, follow-on work will drift into conflicting assumptions:

- discovery roots will blur user, agent, and workspace scope
- every agent will see the same user-level library by accident
- prompt catalog visibility will be confused with full activation
- future compaction will not know whether a skill was actually in use
- runtime state will not know whether to restore a skill after resume

`Holon` needs a skill contract before adding phase-1 skill implementation.

## Non-Goals

This RFC does not define:

- automatic skill recommendation quality
- remote or marketplace-backed skills
- a final model-callable `activate_skill` tool
- skill-provided sandbox or execution authority
- hierarchical skill references beyond one skill package
- every future inheritance rule for delegated work

It only defines the first stable local-first skill contract.

## Design Principles

### A skill is a file-rooted local package

The required entrypoint is `SKILL.md`.

Supporting files such as `references/`, `scripts/`, or `assets/` may exist
next to it, but `SKILL.md` remains the package root.

### Discovery, attachment, and activation are different concerns

Knowing that a skill exists is not the same thing as making it available to one
agent, and that is not the same thing as using it for the current task.

### Catalogs should stay small in prompt terms

The runtime may list many skills, but skill bodies should not be injected just
because they were discovered.

### User-level skill libraries are not ambient state

The `default agent` may see user-level skills by default, but other agents
should not inherit that entire library automatically.

### File-based usage is acceptable

`Holon` does not need to force a dedicated `load_skill` tool in the first
phase.

If an agent sees a catalog entry and decides a skill is needed, it may read the
corresponding `SKILL.md`.

### Activation should be observable

Even if skills are file-based, the runtime still needs a separate activation
record so it can explain current behavior and preserve relevant state across
compaction and resume.

## Terminology

### `skill root`

A directory that contains one or more skill packages.

Each package is expected at:

- `<skill root>/<skill name>/SKILL.md`

### `skill catalog`

The discovered set of skills visible from one scope.

A catalog entry contains metadata such as:

- `skill_id`
- `name`
- `description`
- `path`
- `scope`

### `attached skill`

A skill that one agent is allowed to use.

An attached skill is visible to that agent's runtime logic, but it is not
automatically injected into every turn.

### `active skill`

A skill that has crossed an observable activation boundary for the current turn
or session.

## Discovery Roots

`Holon` should support three local discovery scopes.

### 1. User-level library

User-level skill roots should be:

- `~/.agents/skills`
- fallback to `~/.codex/skills`
- fallback to `~/.claude/skills`

`~/skills` should not be used as a default root.

This scope is a shared library, not ambient prompt state.

### 2. Agent-level root

Agent-level skill roots should be searched under `agent_home`:

- `<agent_home>/.agents/skills`
- fallback to `<agent_home>/.codex/skills`
- fallback to `<agent_home>/.claude/skills`

### 3. Workspace-level root

Workspace-level skill roots should be searched under `workspace_anchor`:

- `<workspace_anchor>/.agents/skills`
- fallback to `<workspace_anchor>/.codex/skills`
- fallback to `<workspace_anchor>/.claude/skills`

## Discovery Order And Root Selection

Within one scope, `Holon` should use the first existing root in this order:

1. `.agents/skills`
2. `.codex/skills`
3. `.claude/skills`

It should not merge multiple roots from the same scope in phase 1.

That means:

- if `<workspace_anchor>/.agents/skills` exists, workspace fallback roots are
  ignored
- if `<agent_home>/.agents/skills` exists, agent fallback roots are ignored

This keeps skill identity and precedence easy to explain.

## Visibility Rules

### `default agent`

The `default agent` may discover:

- user-level skill catalog
- agent-level skill catalog
- workspace-level skill catalog

This makes the default agent the natural operator-facing entrypoint for global
skill discovery.

### `named agent`

A named agent should discover only:

- its own agent-level skill catalog
- the active workspace's workspace-level skill catalog

It should not discover user-level skills by default.

### `child agent`

A child agent should follow the same narrow default as a named agent:

- agent-level skills for that delegated identity or execution
- workspace-level skills for its attached workspace

User-level skills should not be ambiently visible.

## Attachment Rules

Discovery alone should not imply global availability.

The first model should distinguish:

- `discoverable`: present in a visible catalog
- `attached`: available to one agent
- `active`: observably in use

### Default attachment behavior

For phase 1:

- workspace-level discovered skills are considered attachable to agents working
  in that workspace
- agent-level discovered skills are attachable to that same agent
- user-level discovered skills are attachable by the `default agent`

### Creating new agents

Future `create agent` flows should support explicit skill attachment such as:

- `create agent reviewer --skill github-review`
- `create agent worker --skills rust-test,gh-ops`

In this contract, specifying a skill at create time means:

`attach this skill to the new agent`

It does not mean:

- inject the skill into every future turn
- implicitly inherit every active skill from the caller

### User-level to agent-level materialization

If a user-level skill should become part of a non-default agent's skill set,
the runtime may materialize or attach it into the new agent's skill root.

The implementation may realize that as:

- a symlink
- a copied package
- a manifest entry

This RFC only requires the semantic boundary:

- user-level library is the source
- agent-level availability is explicit

## Prompt Behavior

### Catalog injection

`Holon` should inject a skills catalog summary into prompt context for the
current agent.

That summary should contain:

- skill name
- description
- file path

It should not inject full `SKILL.md` bodies just because a skill was
discovered.

### File-based skill use

Phase 1 should allow a file-based usage pattern:

- the agent sees a catalog entry
- the agent decides a skill is relevant
- the agent opens that skill's `SKILL.md`
- the agent follows the workflow from that file

This is acceptable because a skill is fundamentally a file-rooted instruction
package.

### Dedicated load tools are optional in phase 1

`Holon` does not need a mandatory `load_skill` or `activate_skill` tool before
local skill usage is useful.

Such a tool may still be added later if the runtime needs:

- tighter policy control
- stronger activation semantics
- cleaner multi-turn inheritance

But phase 1 should not depend on it.

## Activation Model

Runtime activation should be based on observable signals, not guessed model
intent.

### Activation signals

A skill should be considered activated when any of the following happens:

1. the operator or API explicitly selects it
2. the agent reads a known catalog entry's `SKILL.md`
3. the runtime restores or inherits an already active skill into a new turn or
   delegated execution

### Non-signals

The following should not count as activation by themselves:

- catalog visibility
- a skill being listed in prompt context
- a skill name appearing incidentally in user text
- reading a reference file inside a skill package without first activating the
  package entrypoint

### Activation states

The first state model should distinguish:

- `turn_active`
- `session_active`

`turn_active` means the skill was observably used in the current turn.

`session_active` means the runtime should preserve that activation across
compaction or resume.

### Phase 1 promotion rule

Phase 1 should keep promotion explicit and inspectable:

- reading a known catalog entry's `SKILL.md` enters `turn_active`
- successful turn completion promotes current `turn_active` skills to
  `session_active`
- a new turn clears stale `turn_active` records before fresh activation is
  observed

This keeps activation local-first without requiring a dedicated activation
tool.

## Activation Record

For each active skill, the runtime should be able to record at least:

- `skill_id`
- `name`
- `path`
- `scope`
- `agent_id`
- `activation_source`
- `activation_state`
- `activated_at_turn`

`activation_source` should support at least:

- `explicit`
- `implicit_from_catalog`
- `restored`
- `inherited`

## Compaction And Resume

Compaction should not try to preserve full `SKILL.md` bodies indefinitely.

Instead, it should preserve:

- which skills are `session_active`
- where they came from
- a short summary if useful

After resume, the runtime may re-read the relevant `SKILL.md` from disk when
the skill is needed again.

This keeps skill continuity while avoiding large prompt replay.

## Inspectability

At minimum, `Holon` should be able to explain:

- which skill catalogs were discovered for this agent
- which skills are merely discoverable
- which skills are attached to this agent
- which skills are currently active
- whether activation was explicit or implicit

This is necessary so skill behavior does not become hidden ambient state.

## Open Questions

The following questions remain open after this RFC:

- should peer agents receive any user-level catalog visibility through explicit
  policy rather than only explicit attachment
- should `session_active` promotion happen only through repetition or also
  through explicit operator confirmation
- should future `activate_skill` tooling update only activation state, or also
  materialize attachment
- should delegated agents get copied activation records by default or only when
  the caller explicitly opts in
