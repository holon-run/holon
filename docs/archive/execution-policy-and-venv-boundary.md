# Execution Policy And Virtual Execution Boundary

## Summary

`Holon` should not treat `execution policy` and `virtual execution environment`
as separate, unrelated topics.

They solve different layers of the same problem:

- `resource authority` defines what kinds of resources matter
- `execution policy` defines which runtime states may touch which resources
- `virtual execution environment` defines which of those rules can actually be
  enforced on real process execution

The key judgment is:

- `Holon` can already enforce many admission and control constraints without a
  virtual execution environment
- `Holon` cannot honestly claim strong process-level resource isolation without
  a virtual execution environment

So the next step should not be:

- write a full execution policy as if all boundaries are already enforceable

It should be:

- define the capability boundary between runtime policy and virtual execution
  enforcement

## Problem

Today `Holon` has made progress on:

- workspace binding
- execution roots
- trust and admission marking
- closure and continuation semantics
- persisted work-queue truth for active and queued work

But there is still an unresolved gap between:

- what the runtime wants to constrain
- and what the host can actually constrain when a real shell command runs

This matters most for `ExecCommand`-style behavior.

Without a virtual execution environment, a subprocess can still:

- read unexpected host paths
- write outside intended boundaries
- access host environment variables
- reach the network
- spawn child processes with the same host-level access
- observe or mutate resources the runtime cannot fully see

So if `Holon` writes an execution policy that says:

- this agent is read-only
- this input cannot touch secrets
- this task cannot access the network

those claims are only reliable if the backend can actually enforce them.

## Scope

This RFC defines:

- the resource classes `Holon` cares about
- the difference between admission/control constraints and execution/resource
  constraints
- which constraints can already be enforced by the runtime
- which constraints require a virtual execution environment
- how phase-1 `host_local` language should avoid blocking future
  backend-mediated execution shapes such as `container`, `ssh_remote`, or
  `copied_local`

This RFC does not define:

- a final sandbox backend
- a final container backend
- a final remote executor
- the full profile matrix for all future agent/task kinds
- a complete backend-neutral path model
- a full `ExecutionBackend` runtime abstraction
- multi-backend routing in the current implementation

## Three Layers

`Holon` should use three explicit layers.

### 1. Resource Authority

This is the semantic layer.

It answers:

- what resources exist
- what kinds of contact with those resources matter

Suggested phase-1 resource classes:

- `workspace_read`
- `workspace_write`
- `execution_root_manage`
- `agent_state_mutate`
- `control_plane_mutate`
- `callback_surface_use`
- `network_access`
- `secret_access`

This layer should be defined without reference to a specific sandbox backend.

### 2. Execution Policy

This is the runtime contract layer.

It answers:

- which runtime states are allowed to touch which resource classes
- which authority comes from:
  - admission context
  - trust marking
  - agent kind
  - workspace binding
  - execution root binding
  - future profile selection

This layer should not pretend to provide stronger guarantees than the current
backend can actually enforce.

### 3. Virtual Execution Environment

This is the enforcement layer.

It answers:

- which execution-policy rules can be turned into real process/file/network
  constraints
- what backend is used to enforce them:
  - host-local execution
  - copied workspace
  - local restricted backend
  - container backend
  - remote backend

This is not the policy itself.
It is the backend that makes policy real.

## Future Backend Compatibility

Phase 1 should be `host_local`-first, but it should not be `host_local`-only
in the contract language.

That means:

- the current implementation may continue to assume `host_local`
- the public contract should avoid assuming that every workspace is a host
  filesystem path forever
- the public contract should avoid assuming that every process surface is a
  host-local shell forever

Future execution shapes may include:

- `container`
- `ssh_remote`
- `copied_local`

This RFC does not require `Holon` to implement those backends now.
It only requires phase-1 policy language to avoid foreclosing them.

The practical rule is:

- keep `workspace_entry`, `projection`, and resource classes as logical terms
- treat `host_local` as the only implemented backend in phase 1
- defer a full backend abstraction until the first non-`host_local` backend is
  actually needed

## Phase-1 Resource Classes

For the next implementation phase, `Holon` should keep the resource model
small.

The most useful phase-1 classes are:

- `message_ingress`
- `agent_state`
- `control_plane`
- `workspace_projection`
- `process_execution`

This is intentionally smaller than the eventual long-term resource matrix.

### Why These Six

The first five are resources the runtime can already reason about and enforce
meaningfully:

- `message_ingress`
  - who can enqueue work
- `agent_state`
  - who can mutate runtime liveness and local state
- `control_plane`
  - who can create timers, tasks, callbacks, or other control mutations
- `workspace_projection`
  - which workspace or execution root the agent is attached to

`process_execution` is different.

It still deserves a first-class slot because execution is central to `Holon`,
but phase 1 should treat it conservatively until a stronger backend exists.

## Phase-1 Process Execution Contract

Without a virtual execution backend, `Holon` should make only conservative
claims about `process_execution`.

### Phase-1 Can Honestly Promise

- the runtime knows which `execution_root` a process is launched from
- the runtime can decide whether a process surface is exposed at all
- the runtime can record provenance, admission context, and execution root for
  process launches
- the runtime can use `git_worktree_root` projection for coding workflows when
  the attached workspace is a git repository

### Phase-1 Must Not Over-Promise

- true process-level read-only guarantees
- true process-level write confinement
- reliable network confinement
- reliable secret isolation
- reliable child-process containment

So phase 1 should treat process execution as:

- exposed
- attributed
- projected
- gated

but not yet strongly sandboxed.

## Phase-1 Backend Contract

The first useful virtual execution backend for `Holon` should be:

- `host_local`

This should come before:

- copied workspace execution
- container-backed execution
- remote execution

### Why Start With Host-Local

`Holon` already has real runtime concepts that align with host-local
execution:

- `workspace_anchor`
- `execution_root`
- managed worktrees as a git-specific projection
- reviewable development artifacts in local repositories

So the first step does not need a brand-new execution backend.
It needs a clear contract for the backend `Holon` already effectively uses:
local host execution rooted in an attached workspace.

### Why Not Start With Copied Workspaces

Copied workspaces introduce more complexity immediately:

- sync and drift from canonical repo state
- unclear git identity and branch semantics
- more confusing operator/debug surface
- more complexity before `Holon` has unified execution-root semantics

They may still become useful later, but they are a worse phase-1 backend.

### Phase-1 Projections Under `host_local`

The `host_local` backend should support projections rather than treating
`git worktree` as a backend of its own.

The useful phase-1 projections are:

- `canonical_root`
  - the attached workspace's canonical root directory
- `git_worktree_root`
  - a git-specific derived root created from the canonical repository

This matters because not every workspace is a git repository.

- non-git workspaces may only support `canonical_root`
- git workspaces may support both `canonical_root` and `git_worktree_root`

`git_worktree_root` is therefore a git-specific projection under `host_local`,
not a general-purpose virtual execution backend.

## Phase-1 Host-Local Contract

The phase-1 backend should not be described as a full sandbox.

Its main goal is:

- execution-root consistency
- shared host-local enforcement vocabulary across process, control, and
  worktree-artifact surfaces

### Objects

The minimum phase-1 backend model should include:

- `workspace_anchor`
  - the canonical project root
- `execution_root`
  - the actual root used for current file/process execution
- `execution_mode`
  - `host_local`
- `execution_projection`
  - `canonical_root`
  - `git_worktree_root`
- `projection_kind`
  - `canonical_root`
  - `git_worktree_root`
- `access_mode`
  - `shared_read`
  - `exclusive_write`

The important distinction is:

- `workspace_anchor` preserves project identity
- `execution_root` preserves execution locality

They should not collapse into the same concept.

Phase-1 host-local enforcement should also follow two implementation rules:

- gates for `process_execution`, background task control, and
  `git_worktree_root` availability should come from one shared host-local
  boundary helper, not from scattered per-call checks
- retained worktree artifacts should be controlled according to their artifact
  metadata; a caller does not need to remain inside a `git_worktree_root`
  entry just to discard or review a retained worktree

Phase 1 should reuse the same vocabulary already stabilized in the runtime and
workspace-entry RFC:

- `projection_kind = canonical_root | git_worktree_root`
- `access_mode = shared_read | exclusive_write`
- a derived execution-policy snapshot that reports which guarantees are:
  - `hard_enforced`
  - `runtime_shaped`
  - `not_enforced`

### Core Rules

#### 1. One Active Execution Root Per Agent

At any moment, one agent should have one active `execution_root`.

All file and process surfaces should derive from that same root.

That means:

- file tools should not look at one root while shell tools look at another
- worktree flow should not leave hidden shared-root fallbacks behind

#### 2. Managed Worktree Means Worktree Root Is Real

When an agent is using `git_worktree_root` projection:

- the worktree path is the real `execution_root`

This should be a runtime fact, not just a workflow suggestion.

#### 3. Process CWD Must Be Runtime-Controlled

Subprocess execution should start from the runtime-controlled `execution_root`,
not from:

- daemon startup cwd
- host process cwd
- previous shell-side `cd` side effects

#### 4. Project Identity Still Comes From Workspace Anchor

Instructions, workspace-local skills, and project identity should still bind to
`workspace_anchor`, not to the transient shell cwd.

#### 5. Worktree Is A Git-Specific Projection

`git worktree` should be treated as one projection under the `host_local`
backend, not as a one-off special case for coding workflows and not as a
general-purpose backend by itself.

## Phase-1 Acceptance Criteria

The first backend phase should be considered successful only if:

- an agent running in `git_worktree_root` projection executes all file/process
  surfaces against that root consistently
- prompt/debug/status surfaces can report the active execution mode,
  projection, and root
- `Holon` no longer mixes canonical-root and git-worktree-root semantics
  inside one active execution session
- operators can preserve, review, and clean up worktree-backed results as
  intentional runtime artifacts

## Implication For Future Policy Work

This RFC implies a sequencing rule:

- do not write strong execution-policy promises that assume process isolation
  before the backend can actually provide them

The more honest order is:

1. define resource authority
2. define policy semantics
3. define which parts are runtime-enforceable today
4. let the virtual execution backend make stronger process guarantees real

## The Critical Split

The most important split is:

- `admission/control constraints`
- `execution/resource constraints`

### Admission / Control Constraints

These are constraints the runtime can already enforce without a virtual
execution environment.

Examples:

- whether an ingress surface can enqueue a message
- whether a route can mutate control-plane state
- whether an input is marked as operator, integration, or runtime-owned
- whether a named agent can be implicitly created
- whether a workspace can be attached or switched
- whether a callback can target a given agent

These are real runtime-side guarantees.

### Execution / Resource Constraints

These are constraints that depend on what a running process can actually touch.

Examples:

- whether a shell command can only read inside one root
- whether a shell command can only write inside one root
- whether a shell command can reach the network
- whether a shell command can read secrets
- whether subprocesses inherit the same limits

Without a virtual execution environment, these guarantees are weak or absent.

## Current Capability Map

`Holon` should explicitly distinguish three buckets.

### Hard-Enforced Today

These can already be enforced by the runtime:

- message admission provenance
- control-route authorization
- callback token ownership
- explicit workspace attachment
- default-agent vs named-agent identity rules
- continuation and closure state transitions
- persisted work-item and work-plan updates

### Soft-Enforced Today

These can be signaled or shaped today, but are not hard process guarantees:

- intended execution root
- intended read-only vs writable posture
- intended worktree isolation
- intended trust-sensitive shell behavior
- prompt-level handling of untrusted input

These are useful, but should not be marketed as hard sandbox guarantees.

### Requires Virtual Execution Environment

These should not be treated as reliable guarantees until a suitable backend
exists:

- path confinement for arbitrary subprocesses
- true read-only process execution
- network confinement
- secret isolation from host environment
- child-process inheritance of limits
- strong visibility into actual process resource usage

## Why Resource Classes Must Come Before Task Classes

`Holon` should avoid defining policy mainly in terms of task names such as:

- subagent task
- command task
- repair task
- review task

Task kinds are workflow shapes.
They are not the underlying resource boundary.

The more stable question is:

- what resources can this runtime state touch

For example:

- a mail triage task and a coding task may both create background work
- but their real difference is in resource authority, not in the word `task`

So future execution policy should primarily map to resource classes, not task
labels.

## Candidate Virtual Execution Capabilities

Before choosing a backend, `Holon` should evaluate candidate virtual execution
environments against a small capability map.

The key capability questions are:

### 1. Path Confinement

Can a process be reliably constrained to:

- one workspace root
- one worktree root
- one copied root

### 2. Read / Write Distinction

Can the backend reliably make a process:

- read-only
- writable only within an allowed root

### 3. Network Confinement

Can the backend:

- disable network entirely
- or selectively allow it

### 4. Secret Isolation

Can the backend stop a process from seeing:

- host env vars
- auth files
- token stores
- unrelated credential material

### 5. Process Containment

Do child processes inherit the same constraints?

### 6. Workspace Projection

Can the backend express:

- canonical root
- git worktree root
- copied/snapshotted root

under one runtime model?

### 7. Observability

Can the runtime know enough about what the process touched to support:

- auditability
- debugging
- future policy refinement

## Phase-1 Product Contract

Before a real virtual execution environment exists, `Holon` should adopt a
conservative public contract.

It can honestly claim:

- explicit provenance and admission marking
- explicit workspace and execution-root binding
- explicit control-plane and callback authorization surfaces
- explicit runtime state for closure, continuation, and work-queue truth

It should not over-claim:

- full shell read isolation
- full shell write isolation
- true network sandboxing
- strong secret isolation

Those should remain preview or future-facing until backed by a real virtual
execution environment.

## Phase-2 Direction

Once `Holon` has a usable virtual execution backend, the next step should be:

1. bind resource authority to explicit execution profiles
2. map profiles onto execution roots and workspace projections
3. enforce read/write/network/secret constraints through the backend
4. keep admission/control rules in the runtime layer
5. expose only the guarantees that the backend can actually uphold

This preserves a clean separation:

- runtime decides policy
- execution backend enforces process reality

## Open Questions

The next discussion should focus on:

1. Which phase-1 resource classes are truly necessary for `Holon`?
2. Which of those classes can be enforced today without a virtual execution
   environment?
3. What is the smallest useful virtual execution backend `Holon` could adopt
   first?
4. Which guarantees should stay explicitly soft until that backend exists?

## Decision

`Holon` should not treat execution policy as a fully independent track from
virtual execution.

The correct sequence is:

1. define resource authority
2. define execution policy in terms of those resources
3. make the policy honest about which guarantees require a virtual execution
   environment
4. use the virtual execution environment as the enforcement backend for the
   guarantees that matter at process level
