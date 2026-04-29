# Workspace Binding And Instruction Loading

This RFC defines the first explicit workspace model for `Holon`.

The immediate goal is to support:

- long-lived daemon agents
- explicit workspace attachment
- stable instruction loading
- future `AGENTS.md` and skill loading
- worktree-aware execution without letting shell side effects redefine the
  runtime's project boundary

This document is intentionally narrow. It does not define every future profile,
policy, or UI surface. It defines the first runtime contract for:

- host-owned workspace entries
- agent-owned home directories
- execution roots
- `cwd`

and how those states control instruction loading.

## Short Answer

`Holon` should not treat agent home as the workspace root.

Instead, the first runtime model should be:

- `holon_home`: runtime host state
- `workspace_registry`: host-owned set of known workspaces
- `agent_home`: stable agent identity and persistent state
- `workspace_anchor`: stable root of one workspace entry
- `active_workspace`: the workspace entry selected for the current execution
- `execution_root`: concrete filesystem root for file and shell work
- `cwd`: current working directory inside the execution root

The main rule is:

`Shell execution may vary cwd, but shell side effects must not implicitly redefine workspace attachment, instruction roots, or write authority.`

## Why This RFC Exists

`Holon` is moving toward:

- daemon / `serve` mode
- long-lived agents
- managed worktree flows
- project-level instructions and skills

Without an explicit directory model, those concerns will collide:

- daemon startup cwd will accidentally become project identity
- shell `cd` will silently mutate the next turn's effective root
- worktree entry will blur project identity and execution projection
- `AGENTS.md` and local skills will not know which directory semantics to
  follow
- multi-agent sharing will not know whether a workspace belongs to the host or
  to one agent
- write coordination will drift into tool heuristics instead of runtime policy

The runtime needs a directory model before adding `AGENTS.md` and skill loading
to the public contract.

## Non-Goals

This RFC does not define:

- automatic skill selection
- remote skill backends
- container or VM workspace backends
- hierarchical `AGENTS.md` traversal rules
- every future worktree lifecycle detail

It only defines the first stable runtime concepts needed to support local-first
daemon agents.

## Design Principles

### Host-owned workspaces and agent-owned state are different concerns

`Holon` is a multi-agent runtime host. A workspace should be a host-level
resource that agents attach to, not a private directory implicitly owned by one
agent.

### Stable identity and mutable execution are different concerns

An agent's durable state should not be confused with the project it is
currently allowed to operate on.

### Shell side effects are weak evidence

A shell command can `cd`, but that does not make shell state the source of
truth for runtime workspace attachment.

### Instructions need a stable anchor

Project instructions and local skills should not drift just because the agent
visited a subdirectory or ran a command that changed shell cwd.

### Worktree is an execution projection, not a new agent identity

A managed worktree may become the active execution root, but that does not mean
the agent has become a different project.

### Write authority should come from policy and enforcement

The runtime should not guess whether a shell command is read-only or mutating
and then use that guess as the source of truth.

Write authority should instead come from:

- execution policy
- execution-root attachment
- filesystem enforcement

## Model

### 1. `holon_home`

`holon_home` is the stable home for the runtime host.

It is the place for:

- daemon-level state
- host-level configuration
- workspace registry metadata
- agent registry metadata

It is not a project workspace root.

### 2. `workspace_registry`

`workspace_registry` is the host-owned set of known workspace entries.

Each workspace entry has a stable identity and at minimum includes:

- `workspace_id`
- `workspace_anchor`
- optional metadata such as repo identity or default policy

`workspace_anchor` is not a global default workspace.

It is the stable root of one workspace entry.

The host may manage many such entries.

### 3. `agent_home`

`agent_home` is the stable home directory for one agent.

It is the place for:

- persisted state
- queue and inbox state
- transcripts
- logs
- cache
- agent-scoped `AGENTS.md`
- agent-scoped skills

It is not the project workspace root.

An agent may live for a long time and work across multiple projects without
those projects being redefined as its home.

### 4. `attached_workspaces` and `active_workspace`

An agent may attach to one or more host-managed workspace entries.

This RFC defines two runtime states:

- `attached_workspaces`: the set of workspace entries the agent may use
- `active_workspace`: the workspace entry selected for the current execution

This keeps workspace identity host-owned while letting one agent move across
multiple projects over time.

### 5. `workspace_anchor`

`workspace_anchor` is the stable root of the current `active_workspace`.

It answers:

- what project this work belongs to
- where workspace-level instructions are rooted
- where workspace-local skills are discovered
- which attached project the current execution boundary belongs to

This is the directory that should be used as the logical project identity.

It should remain stable across ordinary shell `cd`.

### 6. `execution_root`

`execution_root` is the concrete filesystem root used for file and shell
execution.

By default:

- `execution_root == workspace_anchor`

In managed worktree flows:

- `execution_root` may become the worktree path

This is the root that tools should treat as the current execution projection.

Multiple agents may share the same `workspace_anchor` while using different
`execution_root` values.

This is the main way `Holon` should support multi-agent work on one project
without collapsing project identity and execution isolation.

### 7. `cwd`

`cwd` is the current working directory for shell and other path-sensitive
execution inside the execution root.

It must satisfy:

- `cwd` is inside `execution_root`

`cwd` may change more often than `workspace_anchor` or `execution_root`.

### 8. `execution_policy`

`execution_policy` determines whether the current execution root is being used
in a read-only or writable way.

This RFC does not define the full policy matrix. It only defines one key
boundary:

- writeability is determined by runtime policy plus filesystem enforcement
- writeability is not determined by tool-specific command parsing

## Invariants

The runtime should preserve these invariants:

- every host has exactly one `holon_home`
- every workspace entry has exactly one `workspace_anchor`
- every agent has exactly one `agent_home`
- every execution has exactly one `active_workspace`
- every execution has exactly one `execution_root`
- `execution_root` must belong to the selected workspace entry's allowed
  projection
- `cwd` must be within `execution_root`
- shell side effects alone must not change `workspace_anchor`
- shell side effects alone must not change instruction roots
- shell side effects alone must not upgrade write authority

## Why `agent_home` Must Not Equal `workspace_anchor`

This is the most important rejection in the RFC.

If `agent_home` is treated as the base workspace root, the model breaks down
quickly:

- a long-lived agent that works across repos no longer has a meaningful project
  root
- workspace `AGENTS.md` and local skills would resolve against the wrong place
- file authority would become confused with private agent state
- daemon startup layout would leak into project identity

`agent_home` is about agent identity.

`workspace_anchor` is about project identity.

They must stay separate.

## Why Workspaces Are Host-Owned, Not Agent-Owned

`Holon` is a multi-agent runtime host.

That means a workspace is not naturally "owned" by one agent. The same project
may need:

- one analysis agent
- one coding agent
- one review agent
- one agent resumed from a previous task

Those agents may share the same project attachment while keeping separate:

- memory
- queues
- transcripts
- execution roots
- execution policy

The runtime should therefore model workspaces as host-owned entries that
agents attach to.

## Runtime Behavior

### `holon run`

`run` is allowed to use a convenience default.

For v1:

- if `--workspace-root` is provided, use it as the `workspace_anchor` for an
  ephemeral workspace entry
- otherwise use the invocation cwd as the anchor for an ephemeral workspace
  entry
- attach the run to that workspace entry as `active_workspace`
- set `execution_root = workspace_anchor`
- set `cwd` to the invocation cwd if it is inside the anchor, otherwise the
  anchor itself

This keeps `run` simple while still making the model explicit.

When `run` targets a persistent agent with `--agent` and omits new workspace
flags, it should preserve that agent's current active workspace or worktree
binding instead of silently rebinding to a different anchor.

This RFC does not require automatic promotion from subdirectory to git repo
root. That can be introduced later as an explicit policy, not an invisible
default.

### `holon serve`

`serve` must not derive project identity from daemon process cwd.

For v1:

- daemon startup cwd has no project meaning by itself
- coding-capable agents must be created or resumed with an explicit attached
  workspace entry
- agents without a workspace attachment may still exist, but should not assume
  local file/shell authority

This is the minimum rule that prevents daemon process state from leaking into
project attachment.

## Worktree Behavior

Managed worktree flow should be modeled as:

- `workspace_anchor` remains stable
- `execution_root` changes to the worktree path
- `cwd` is re-rooted into the worktree

This keeps project identity stable while allowing execution isolation.

The first version should not treat worktree entry as a silent project identity
change.

## Shell Behavior

`Holon` should use explicit runtime cwd and explicit workspace attachment.

That means:

- shell calls may accept an explicit per-call cwd
- shell execution always runs with an explicit runtime-provided cwd
- a shell command that performs `cd` does not, by itself, change the runtime's
  `workspace_anchor`
- a shell command that performs `cd` does not, by itself, change instruction
  loading roots
- a shell command that performs `cd` does not, by itself, upgrade write
  authority

If `Holon` later wants a user-visible "change working directory" behavior, it
should be an explicit runtime action, not an inferred shell side effect.

## Execution Policy And Filesystem Enforcement

This RFC adds one more boundary rule.

`Holon` should not decide whether a shell call is "read" or "write" by trying
to fully understand the command text.

That approach is too brittle for:

- shell commands
- scripts
- interpreters
- build tools

Instead, the runtime should use:

- workspace attachment
- execution-root selection
- execution policy
- filesystem enforcement

as the source of truth.

The intended shape is:

- a workspace entry may be shared by many agents
- an execution root may be read-only or writable depending on policy
- mutating authority is granted at the execution-root level
- tools run inside that authority boundary

This keeps write coordination in runtime policy, not in tool heuristics.

## Instruction Loading

This RFC also defines the first instruction loading contract.

### Agent-Level Instructions

Agent-scoped instructions should load from:

- `<agent_home>/AGENTS.md`

Purpose:

- long-lived agent behavior
- durable preferences
- agent-specific operating style

This is not a repo contract.

### Workspace-Level Instructions

Workspace-scoped instructions should load from:

- `<workspace_anchor>/AGENTS.md`

Compatibility fallback may later allow:

- `<workspace_anchor>/CLAUDE.md`

but fallback should only apply when `AGENTS.md` is absent, not as a second file
loaded in parallel.

Purpose:

- project-specific rules
- verification commands
- repo conventions
- non-obvious workflow constraints

This is not agent identity.

### Why Workspace Instructions Follow `workspace_anchor`, Not `cwd`

This is deliberate.

If instructions follow plain `cwd`, then:

- visiting a subdirectory changes the effective project contract
- shell `cd` risks mutating prompt assembly
- long-lived agents become difficult to reason about

Instruction roots should be stable across ordinary execution movement.

### Why Workspace Instructions Do Not Default To `execution_root`

Managed worktree is an execution projection, not automatically a new logical
project.

Pinning workspace instruction discovery to `workspace_anchor` gives the runtime
a stable contract across:

- original workspace
- managed worktree projection
- resumed sessions

Future work may add an explicit opt-in policy for "follow worktree-local
instructions", but that should not be the default public behavior.

## Skill Loading

This RFC defines only the workspace and loading roots that later local
instruction and skill discovery depend on.

### Agent-Scoped Skills

Discover from:

- `<agent_home>/.agents/skills/*/SKILL.md`

Compatibility fallbacks may later allow, in order:

- `<agent_home>/.codex/skills/*/SKILL.md`
- `<agent_home>/.claude/skills/*/SKILL.md`

### Workspace-Scoped Skills

Discover from:

- `<workspace_anchor>/.agents/skills/*/SKILL.md`

Compatibility fallbacks may later allow, in order:

- `<workspace_anchor>/.codex/skills/*/SKILL.md`
- `<workspace_anchor>/.claude/skills/*/SKILL.md`

### Activation Rule

Phase 1 should use explicit activation only.

Discovery and activation are separate:

- discovery builds a catalog
- activation decides what enters the prompt

The runtime should not inject every skill into every turn.

When more than one compatibility path exists, the first existing directory in
the declared order should be used as the local skill root for that scope.

## Prompt Assembly Order

When these sources exist, the current runtime contract order is:

1. runtime hard constraints
2. execution and trust contract
3. mode guidance
4. agent-scoped instructions
5. workspace-scoped instructions
6. active skills
7. tool guidance

This order keeps stable runtime policy first while letting project rules remain
more specific than agent persona.

## Inspectability

This model should be visible to operators.

At minimum, prompt inspection or debug surfaces should show:

- `workspace_id`
- `agent_home`
- `workspace_anchor`
- `execution_root`
- `cwd`
- `execution_policy`
- loaded agent instruction path
- loaded workspace instruction path
- activated skill ids and source paths

Without this, instruction loading will become a hidden source of confusion.

## Open Questions

The following questions remain open after this RFC:

- should `run` gain an explicit `--cwd` separate from `--workspace-root`
- should worktree flows support an opt-in "follow active root for workspace
  instructions" mode
- should workspace instruction discovery eventually support hierarchical
  traversal from anchor to cwd
- should explicit runtime actions exist for changing `cwd` without changing
  workspace attachment
- should one execution attach to multiple workspaces at once, or should
  cross-workspace work always be decomposed into subtasks
- what is the first explicit writable/read-only execution policy surface

These are important, but they do not block the first contract.

## Decision

Holon should model host-owned workspace entries, agent-owned homes, execution
roots, and current working directory as separate runtime states.

`AGENTS.md` and workspace-local skills should anchor to the stable workspace
attachment, not to daemon startup cwd and not to shell side effects.

Write authority should be derived from execution policy plus filesystem
enforcement, not from tool-specific command classification.
