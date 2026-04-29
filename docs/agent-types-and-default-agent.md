# Agent Types And Default Agent

This RFC defines the next agent model for `Holon`.

The immediate goal is to stop overloading one word, "agent", with several
different concerns:

- default operator entrypoint
- long-lived named runtime
- bounded delegated child work
- externally reachable versus private execution
- durable versus ephemeral lifecycle

`Holon` already has:

- a long-lived multi-agent host
- a default operator-facing agent
- explicitly targeted non-default agents
- delegated child work supervised by `child_agent_task`

Without a clearer model, follow-on work such as skills, workspace attachment,
callbacks, and agent creation will keep drifting.

## Short Answer

`Holon` should use `default agent` as the primary term, not `root agent`.

`Holon` should also treat `agent` as the primary runtime primitive and describe
other distinctions with explicit axes instead of one overloaded role list.

The first stable model should distinguish:

- `default agent`: the host's default operator-facing agent
- `named agent`: an explicitly created long-lived non-default agent
- `child agent`: an agent created from another agent for delegated work

And each agent should also have explicit lifecycle properties:

- `visibility`: `public` or `private`
- `durability`: `persistent` or `ephemeral`

The main rules are:

- `default` means default entrypoint, not elevated authority
- `child` means parent provenance, not necessarily persistence
- `delegated execution` is best understood as a child-agent lifecycle pattern,
  not a separate runtime primitive

## Why This RFC Exists

Several upcoming features depend on a stable answer to "which agent are we
talking about?":

- skill libraries and agent-local skill roots
- workspace attachment defaults
- CLI routing when `--agent` is omitted
- delegation and inheritance boundaries
- callback and webhook wake routing
- future agent creation surfaces

If `Holon` does not define these concepts explicitly, it becomes too easy to
accidentally treat the default agent as:

- a privileged agent
- a globally shared prompt state
- the owner of every workspace
- the implicit parent of every future agent

Those are different concerns and should remain separate.

## Non-Goals

This RFC does not define:

- the full sandbox or execution-profile model
- the full task/delegation lifecycle
- the final public UX for creating agents
- the final cleanup policy for ephemeral child agents
- a user-facing `name` field separate from `id`

It only defines the first stable agent model for default and non-default
agents.

## Naming Decision

`Holon` should use `default agent` as the user-facing and runtime term.

It should not use `root agent` as the primary term.

Reason:

- `root` strongly implies elevated privilege
- `Holon` does not intend the default agent to bypass trust or execution rules
- the implementation already uses `default_agent_id`, so `default agent` keeps
  naming aligned with current runtime state

`root` may still be used for unrelated concepts such as root directories or
root tasks, but not as the primary agent-role term.

## Primary Runtime Primitive

`Holon` should treat `agent` as the primary runtime object.

Each agent owns one execution context, including at minimum:

- queue
- lifecycle state
- brief and transcript state
- waiting intents and external triggers
- workspace attachments
- agent-scoped `AGENTS.md`
- agent-scoped skill roots
- `agent_home`
- stable `agent_id`

This keeps "new long-lived context" and "delegated child work" on one substrate
instead of splitting them into unrelated primitives.

## Agent Kinds

### 1. `default agent`

The `default agent` is the host's primary operator-facing agent.

It is:

- the default target when CLI commands omit `--agent`
- the default long-lived continuity surface for a host
- the agent most likely to receive operator prompts first

It is not:

- a privileged agent
- the owner of all workspaces
- the automatic parent of every future agent
- the source of implicit prompt inheritance for all other agents

### 2. `named agent`

A `named agent` is an explicitly created long-lived non-default agent.

It has:

- its own `agent_home`
- its own queue, transcript, briefs, and state
- its own workspace attachments
- its own activated skills and local instruction state
- an explicitly chosen, meaningful `agent_id`

It should be treated as a sibling of the default agent, not as a child of the
default agent.

This RFC uses `named agent` instead of `peer agent` because the important
semantic distinction is explicit identity, not just sibling-ness.

### 3. `child agent`

A `child agent` is an agent created from another agent to handle delegated
work.

What makes it a child agent is not whether it is persistent. What matters is:

- it has parent provenance
- it serves a narrower delegated objective
- it should not inherit the full ambient state of the caller

A child agent may later be realized as:

- a private ephemeral child
- a private persistent child
- a public persistent child

The lifecycle choice does not change the fact that it is a child agent.

## Agent Axes

The three kinds above are not enough by themselves. `Holon` should also model
these additional axes explicitly.

### Visibility

- `public`
  - externally routable by normal agent-targeting surfaces such as CLI or HTTP
- `private`
  - not generally operator-targetable
  - may still be resumed by runtime-owned callback or wake routing

This distinction matters because a child agent may need to wait on callbacks
without becoming a first-class public control surface.

### Durability

- `persistent`
  - keeps local state until explicitly removed
- `ephemeral`
  - created for bounded work and eligible for cleanup after closure

Ephemeral does not mean "no state". Even an ephemeral child agent still has its
own queue, waiting state, transcript, and `agent_home` while it exists.

## Delegated Execution

`Holon` should stop treating "delegated" as a separate primitive alongside
agents.

Instead:

- delegated work should be understood as a child-agent execution pattern
- `child_agent_task` is the narrow runtime supervision record for that pattern

Legacy `subagent_task` records may still appear in old local state, but new
runtime-created delegated child work uses `child_agent_task`.

The intended long-term shape is:

- `agent` remains the runtime primitive
- bounded delegation becomes `child agent + lifecycle policy`

This avoids forcing callers to choose between two overlapping concepts such as
"worker agent" versus "delegated execution".

## Identity

Every agent should have:

- a stable `agent_id`
- an `agent_home`

This includes child and ephemeral agents.

Reason:

- callback and wake routing need a stable target
- audit and transcript records need stable provenance
- agent-scoped `AGENTS.md` and skills need a stable root
- cleanup should remove a known state root, not an implicit scratch context

### `agent_id`

This RFC distinguishes two cases:

- `default agent` and `named agent`
  - should use meaningful, operator-facing ids
  - examples: `default`, `release-bot`
- `child agent`
  - should normally use runtime-generated ids
  - the exact id format is an implementation detail

The id is the runtime identity and control-plane key.

This RFC does not require a separate user-visible `name` field.

### `agent_home`

Every agent has an `agent_home`.

For persistent agents, it remains as durable local state.

For ephemeral agents, it remains valid for the lifetime of that agent and may
be cleaned up after closure according to later policy.

## Default Routing Rules

The first routing rules should be:

- commands that omit `--agent` target the `default agent`
- explicitly named public agents target that agent and only that agent
- `holon run` without `--agent` should execute on a temporary ephemeral agent
- `holon run --agent <id>` should execute on that named persistent agent
- `holon run --agent <id> --create-agent` may create the named persistent agent
  on first use
- `holon run --agent <id> --create-agent --template <selector>` may initialize
  that named persistent agent from a reusable template
- child-agent creation or delegation surfaces must be explicit

This keeps operator intent predictable.

## State And Inheritance Rules

### The `default agent` is not privileged

The default agent still follows the same trust and execution boundaries as any
other agent.

It is only the default route for operator interaction.

### Named agents do not implicitly inherit all default-agent state

A named agent should not automatically inherit:

- attached workspaces
- activated skills
- loaded `AGENTS.md` guidance
- pending objective state
- current `cwd`

These should require explicit attachment or transfer.

### Child agents inherit only bounded delegation context

A child agent may inherit:

- the delegated objective
- relevant workspace attachment or execution root
- explicitly selected skills or instructions

It should not automatically inherit the full ambient state of the caller.

## Workspace Relationship

This RFC follows the existing workspace model:

- workspaces are host-owned
- agents attach to workspaces
- execution roots are per-execution projections

That means:

- the default agent does not own the host workspace registry
- named agents may attach to the same workspace entries as the default agent
- child agents may receive a narrower execution projection than the caller

## Skill Relationship

This RFC also constrains skill loading:

- user-level skill libraries should not automatically become active for every
  agent
- the default agent may be the default consumer of global skill catalogs
- named and child agents should only receive skills through explicit attach,
  activation, or delegation rules

This keeps "default entrypoint" separate from "global prompt pollution".

## Inspectability

At minimum, agent-facing status surfaces should be able to explain:

- the current agent id
- whether the current agent is the default, named, or child kind
- whether the current agent is public or private
- whether the current agent is persistent or ephemeral
- whether the current execution is delegated
- the parent agent id for child agents
- which workspace attachments belong to this agent

This is necessary so operator expectations match runtime reality.

## Open Questions

The following questions remain open after this RFC:

- should child agents be able to become public, or should only default and
  named agents receive direct external ingress
- should the default agent always be eagerly initialized on host boot, or may
  it be lazily created on first use
- should user-level skill catalogs be visible only to the default agent by
  default
- should named-agent creation require an explicit operator command or also
  allow controlled runtime creation
- when an ephemeral child agent waits on external events, what cleanup rule
  should retire it after closure

These questions matter, but they do not block the first model.

## Decision

`Holon` should use `default agent` as the primary term for the host's default
operator-facing agent.

It should treat `agent` as the primary runtime primitive and model other
distinctions with explicit kinds and lifecycle axes:

- `default agent`
- `named agent`
- `child agent`
- `public` / `private`
- `persistent` / `ephemeral`

The default agent is the default entrypoint for operator interaction, but not a
privileged agent and not the owner of all workspace or prompt state.
