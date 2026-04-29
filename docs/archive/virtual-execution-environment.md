# Virtual Execution Environment

## Core Judgment

If `Holon` wants stronger isolation without forcing users through per-command sandbox prompts,
the right direction is not command-level sandboxing.

The better direction is to introduce a unified `virtual execution environment`.

That means:

- the agent sees a stable OS / filesystem / process abstraction
- the runtime decides how that abstraction is implemented
- the backend may be local, worktree-scoped, container-backed, or remote
- the agent should not need to know which one it is using

This is a better fit for `Holon` than a command-by-command sandbox because it preserves a
clean agent experience while still allowing strong control over what the agent can read,
write, execute, and reach.

## Why A Virtual Filesystem Alone Is Not Enough

A filesystem abstraction helps for:

- `ReadFile`
- `WriteFile`
- `EditFile`
- directory listing
- path boundary checks

But it does not solve the real problem if `ExecCommand` still runs directly on the host.

Right now the current model is still close to:

- file tools are constrained by `workspace_root`
- shell execution still uses real `zsh -lc <command>`
- the shell still has normal host process semantics

That means the agent can still reach:

- absolute paths
- subprocesses
- environment variables
- network
- git / ssh / curl / python and other host tools
- shell redirection, pipes, globbing, symlinks

So a virtual filesystem without virtual process execution would create a misleading sense of
isolation. File tools would be constrained, but the shell would still be real.

The right abstraction is therefore not just `virtual FS`.

It is `virtual execution environment`.

## The Right Split

The clean model is to separate four layers.

### 1. Agent-Facing OS API

The agent should only see capability-level operations such as:

- read file
- write file
- edit file
- list files
- search text
- execute process
- spawn background process
- register callback / receive ingress

At this layer, the agent should not know whether execution is local, remote, or containerized.

### 2. Capability Kernel

Each agent should run with an explicit capability profile.

A profile can define:

- readable roots
- writable roots
- allowed executables or execution classes
- network access
- git access
- background-process permission
- external ingress permission
- callback capability permission

This is the important shift:

- not “decide sandbox policy on every command”
- but “assign the agent a stable execution profile”

That keeps the model simple for both product behavior and implementation.

### 3. Backend Executor

The runtime should be free to choose different implementations behind the same interface:

- local workspace executor
- local worktree executor
- local restricted executor
- container-backed executor
- remote workspace executor
- remote OS executor

This is what makes it possible to eventually run agents against a remote environment without
changing the agent-facing model.

### 4. Runtime Policy

Policy still matters, but it should sit above the backend, not inside every shell invocation.

Examples:

- root agent gets a broader profile
- ephemeral child gets a narrower worktree-scoped profile
- review agent gets read-only access
- remote durable child may be routed to a remote backend by default

This is still policy-driven, but it is profile selection, not per-command friction.

## Why This Fits Holon Better

This direction lines up with the rest of `Holon`'s runtime design.

`Holon` already tends to prefer:

- explicit runtime state
- explicit message types
- explicit trust and origin
- queue-mediated behavior
- stable session or agent semantics

A virtual execution environment matches that style well.

It treats execution as another runtime substrate, not as an ad hoc tool detail.

That is also more aligned with the broader direction we discussed elsewhere:

- `Holon` should become more `agent-first`
- more `runtime-substrate-first`
- less dependent on tool-by-tool special cases

## Why Not A Full Fake POSIX Filesystem

There is one implementation risk worth calling out clearly.

This direction should not begin with a heavyweight fake filesystem layer such as a full userland
POSIX emulation or FUSE-first design.

That path tends to become expensive quickly because many tools assume real filesystem behavior:

- symlinks
- atomic rename
- file watching
- permission bits
- path normalization edge cases
- git expectations
- shell process behavior

If the shell still ultimately runs in a real environment, the fake filesystem layer usually
becomes more complexity than protection.

So the better implementation strategy is:

- define runtime interfaces first
- keep the backend pluggable
- only emulate filesystem behavior where actually necessary

In other words:

- do not start with “build a fake OS”
- start with “abstract file and process hosts behind a stable runtime contract”

## Recommended Interfaces

At a high level, `Holon` should move toward interfaces like:

- `FileHost`
- `ProcessHost`
- `WorkspaceView`
- `IngressHost`

The exact names do not matter much yet. The important point is that:

- file tools stop touching the host filesystem directly
- process tools stop spawning shell commands directly
- both go through the same per-agent execution environment

This allows one agent to run on:

- a local worktree-backed environment
- a read-only local environment
- a remote environment
- a more strongly isolated backend

without changing the tool contract presented to the model.

## Where Execution Profiles Should Bind

The primary binding point for execution profiles should be the `agent`.

But the profile should not be treated as completely static.

The cleaner model is:

- `agent` owns the base execution profile
- `task kind` can narrow that profile temporarily
- `worktree` projects the profile into a concrete workspace view
- `trust` and runtime policy can apply additional filters

In other words:

`effective execution profile = agent base profile + task overlay + workspace projection + policy filters`

This split matters because these runtime objects solve different problems.

### Agent

The agent is the long-lived execution subject.

It owns:

- queue
- state
- sleep / wake
- brief
- callback capability
- long-lived identity

That makes it the natural owner of durable execution permissions.

So capabilities such as:

- readable roots
- writable roots
- execution class
- background process allowance
- network allowance

should primarily belong to the agent.

### Task

A task is a bounded delegation unit.

It is a poor source of truth for durable permission identity, but it is a very good place for
temporary restriction.

Examples:

- review task becomes read-only
- ephemeral child task loses network access
- background worker task loses callback-registration permission

So tasks should be able to narrow the execution environment, but not redefine the long-term
permission model from scratch.

### Worktree

A worktree is not a permission owner.

It is better understood as a workspace projection.

The same agent profile can be projected into:

- the original workspace
- a managed worktree
- a remote workspace root

So worktree should shape path roots and environment view, not become the main place where
execution authority is defined.

## Why This Split Is Better

This gives `Holon` a cleaner long-term model:

- durable identity and durable permissions live with the agent
- short-lived tasks can become more restrictive without inventing a new permission system
- worktree isolation remains an environment mapping, not an alternate actor model

It also fits the broader `agent-first` direction better.

If `Holon` moves toward root agents, ephemeral child agents, and durable named child agents,
the execution story remains coherent:

- every agent has a base profile
- child agents can inherit or receive a narrower profile
- tasks only further narrow behavior
- worktree only changes the concrete workspace surface

## Relationship To Current Holon Tools

Today the tool split is already moving in a clearer direction:

- `tool::spec`
- `tool::dispatch`
- `tool::execute`
- `tool::helpers`

That makes this a good moment to add another layer underneath:

- tool execution should depend on a runtime execution host
- not directly on `tokio::fs` or `tokio::process::Command`

In particular, the current long-term pressure point is `ExecCommand`.

As long as `ExecCommand` remains a thin wrapper around host shell execution, `Holon` does not
really own its execution boundary.

That is the first thing this design should fix.

## Recommended Migration Direction

The migration does not need to happen all at once.

### Phase 1

Introduce execution-host abstractions:

- extract shell execution behind a `ProcessHost`
- extract file access behind a `FileHost`
- keep behavior equivalent to today, but route through interfaces

This phase is mostly structural.

### Phase 2

Add profile-based execution environments:

- read-only profile
- worktree writer profile
- isolated child profile
- background-worker profile

At this point the runtime begins choosing environments by agent role, trust, or task kind.

### Phase 3

Add alternate backends:

- remote executor
- container-backed executor
- stronger local isolation backend

At this point the agent truly stops knowing whether it is operating against the host machine
or another environment.

## Bottom Line

If `Holon` wants low-friction isolation, it should not start with command-level sandbox policy.

It should move toward a `virtual execution environment`:

- stable agent-facing OS capability surface
- profile-based execution control
- pluggable file and process backends
- optional local, worktree, remote, or isolated implementations

That is a cleaner long-term substrate than either:

- raw host execution
- or a command-by-command sandbox approval model

And it fits the broader architecture direction of `Holon` much better.
