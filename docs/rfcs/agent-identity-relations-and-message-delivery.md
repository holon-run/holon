---
title: RFC: Unified Agent Identity, Relations, And Message Delivery
date: 2026-09-06
status: draft
---

# RFC: Unified Agent Identity, Relations, And Message Delivery

## Summary

Holon should treat every agent as the same addressable runtime object.

`public agent` and `private agent` should no longer be product-level identity
classes. A subagent is an agent with explicit lineage, supervision, durability,
lifecycle attachment, and policy relations. It is not a hidden or lesser
runtime species.

The first agent-facing native tool surface should separate two operations that
are currently combined by `SpawnAgent`:

1. `CreateAgent` creates an independently managed agent identity.
2. `InvokeAgent` creates a result-bearing invocation against an existing agent
   or creates a supervised subagent as the invocation target.

The runtime also needs an internal `AgentMessageDeliveryService` that admits an
asynchronous message to an existing authorized agent. The first release does
not expose a general `SendAgentMessage` native tool. Invocation, operator
ingress, and future protocol adapters must reuse the same delivery contract
rather than create private queue paths.

Every adapter must call the same application service. Native runtime tools must
not fork the CLI to implement runtime semantics.

Message delivery is not synchronous RPC. Acceptance means that Holon durably
admitted one message under an identity-lifecycle fence. It does not mean that a
turn completed or that the target produced a result.

This RFC also defines the migration away from:

- `AgentVisibility::{Public, Private}` as an identity classification;
- `AgentOwnership::{ParentSupervised, SelfOwned}` as a bundled lifecycle model;
- `AgentProfilePreset::{PrivateChild, PublicNamed}` as a combined
  create/delegation/capability contract; and
- `SpawnAgent` as the agent-facing surface for creation and delegation.

## Status And Precedence

This RFC is the proposed parent contract for agent identity, agent relations,
and cross-agent message admission.

It preserves:

- the canonical create commit and bootstrap-repair boundary from
  `agent-create-contract.md`;
- the distinction between `Agent`, `WorkItem`, `Task`, and `Waiting` from
  `agent-control-plane-model.md`;
- the bounded delegation and workspace-isolation rules from
  `agent-delegation-tool-plane.md`;
- the asynchronous task and continuation model from
  `agent-actor-invocation-model.md`;
- the `Active -> Deleting -> Deleted` identity lifecycle and tombstone rules
  from `agent-deletion-lifecycle.md`;
- provenance, authority, and admission rules from
  `default-trust-auth-and-control.md`; and
- the tool-plane layering rules from `tool-surface-layering.md`.

When this RFC is accepted, it supersedes these narrower claims:

- `public versus private` is a primary agent lifecycle axis;
- a profile preset determines identity visibility or lifecycle ownership;
- every accepted message to an agent necessarily creates a result-bearing
  actor invocation task;
- `private_child` and `public_named` are the canonical product vocabulary; and
- `SpawnAgent` is the canonical long-term surface for both independent creation
  and supervised delegation.

Historical RFCs remain useful records. Their conflicting terms become legacy
implementation descriptions rather than the current target contract.

## Problem

Holon's current implementation has one underlying agent runtime, but its public
shape bundles several independent decisions:

```text
AgentKind
  default | named | child

AgentVisibility
  public | private

AgentOwnership
  self_owned | parent_supervised

AgentProfilePreset
  public_named | private_child
```

The preset then selects creation behavior, supervision, cleanup ownership, tool
families, result shape, and whether the agent appears as independently
addressable.

This creates four problems.

First, product vocabulary does not match the runtime object model. A supervised
child already has an agent ID, AgentHome, state, messages, tasks, and lifecycle,
but `private` suggests that it has no independent identity or operator-facing
entry.

Second, unrelated policy is inferred from names. Visibility, peer messaging,
operator discoverability, cleanup, persistence, and capability packages must be
able to evolve independently.

Third, `SpawnAgent` mixes two state machines. `private_child` means delegated
work with a supervising task, while `public_named` means independent creation
without a task handle.

Fourth, cross-agent messaging has no dedicated admission contract. Reusing
create or invocation to send a message obscures provenance, lifecycle fences,
idempotency, and result semantics.

## Goals

- make every live agent a first-class, addressable identity;
- represent subagents through explicit relations rather than a private class;
- show subagents in an operator-visible tree with independent detail and
  conversation entry points;
- keep operator discoverability separate from peer-agent authorization;
- separate independent create, result-bearing invocation, and message delivery;
- define an asynchronous, durable, idempotent delivery receipt;
- linearize message admission against stop and delete lifecycle state;
- preserve immutable origin, trust, authority, and causation evidence;
- keep one core service per operation behind all adapters;
- migrate existing records without changing agent IDs, history, lineage,
  supervision results, or deletion tombstones; and
- remove agent-facing `SpawnAgent` when `CreateAgent` and `InvokeAgent` are
  introduced, without a versioned tool compatibility window.

## Non-Goals

- synchronous agent RPC;
- treating message acceptance as task completion;
- replacing `WorkItem`, `Task`, `Waiting`, or continuation semantics;
- making all CLI commands provider tools;
- implementing native tools by spawning `holon` CLI processes;
- granting peer-agent access because an agent is visible to the operator;
- arbitrary per-message tool capability changes;
- redesigning templates, skills, model precedence, or workspace projection;
- defining purge or historical evidence erasure;
- federated identity or remote A2A transport; or
- removing every legacy field in the first implementation change.

## 1. Canonical Agent Identity

Every agent has one stable technical identity:

```text
AgentIdentity
  agent_id
  display_name?
  identity_lifecycle
  created_at
  deleted_at?
```

`agent_id` is the canonical routing and audit key. A display name may be
resolved by an authorized adapter, but receipts and durable evidence always
record the resolved `agent_id`.

The canonical identity does not contain `public` or `private`.

`default` remains a deployment/configuration role: one active agent may be the
configured default ingress target. It is not a separate runtime species.

`child` becomes a derived relation:

```text
is_child(agent) = agent.lineage_parent_agent_id is not null
```

It must not independently decide visibility, lifetime, tools, or message
authority.

## 2. Orthogonal Agent Relations

The runtime should represent the following axes independently.

Canonical relation and policy state is stored as normalized, versioned records
rather than additional bundled identity classification columns. The canonical
agent detail may flatten the current records into one projection, but lineage,
supervision, lifecycle attachment, durability, capability policy, and message
policy retain independent state transitions and audit history.

### 2.1 Lineage

```text
AgentLineage
  child_agent_id
  parent_agent_id
  created_at
  creation_cause
```

Lineage is the durable historical parent-child relation used to build the
operator tree. It does not by itself grant control or message authority.

A canonical agent has at most one lineage parent. The first implementation
treats that edge as immutable historical provenance.

A child keeps its lineage if it later becomes independently managed. Reparenting
is outside the initial scope.

### 2.2 Supervision

```text
AgentSupervision
  supervision_id
  supervisor_agent_id
  child_agent_id
  delegated_from_work_item_id?
  state
```

Supervision is an active lifecycle-management relation. It defines:

- who may stop or delete the supervised child;
- who must resolve the child's lifecycle before supervision closes; and
- which parent-owned WorkItem or task created the obligation when available.

Invocation result routing, follow-up correlation, and task completion belong to
the separate `ActorInvocation` task. Lineage may outlive supervision.
Supervision must not be reconstructed only from lineage or invocation history.

The first implementation allows at most one active lifecycle-owning supervision
relation for a child. Additional observers may hold task or agent references,
but they do not become lifecycle supervisors.

If a supervisor crashes, persisted supervision remains the source of truth and
the parent may resume its decision after recovery. If a supervisor is deleted
while an attached child remains live, the child enters an explicit
`cleanup_required` posture for authorized operator or recovery handling. The
runtime must not silently promote or delete the child.

### 2.3 Durability

Initial values:

```text
AgentDurability
  persistent
  ephemeral
```

Durability describes the intended retention of the live agent identity after
its initiating work closes. It does not bypass the deletion lifecycle.

### 2.4 Lifecycle Attachment

Initial values:

```text
AgentLifecycleAttachment
  independent
  supervision_attached
```

An independently managed agent is stopped or deleted through its own lifecycle
surface.

A supervision-attached agent has a parent-owned lifecycle decision. Completing
or accepting one invocation does not stop or delete the agent. The parent must
explicitly choose a canonical lifecycle action:

- keep the child available for another invocation, leaving supervision active
  and the cleanup obligation unresolved;
- stop the child through the canonical stop lifecycle while retaining active
  supervision and the unresolved cleanup obligation; or
- delete the child through the canonical deletion job, fence, and tombstone
  model, closing supervision as deletion completes.

In the first release, neither keep/reuse nor stop is a supervision-closing
transition. A kept child remains available for invocation. A stopped child
remains `ephemeral` and `supervision_attached`; the parent retains lifecycle
authority and may explicitly start it for later reuse or delete it. The
supervision relation and cleanup obligation remain active in both cases.
Because detach and persist are deferred, deletion completion is the only
first-release transition that closes supervision for an attached child.

Explicit detach or persist transitions are deferred beyond the first release.
The normalized relation model must leave room for those transitions without
requiring a new identity class.

The runtime persists the unresolved cleanup obligation and executes the chosen
transition. Stopping does not satisfy that obligation or transfer lifecycle
ownership. It must not infer deletion merely because an invocation task became
terminal, and it must not use a second archive-only lifecycle.

The default delegated child is:

```text
durability = ephemeral
lifecycle_attachment = supervision_attached
```

Giving that child an operator-visible entry does not detach it or make it
persistent. `ephemeral` expresses the parent's retention intent and cleanup
obligation; it is not an automatic task-completion deletion trigger.

### 2.5 Capability And Message Policy

Capability policy answers which stable tool families an agent may use.

Message policy answers which principals may discover or send messages to the
agent. The first implementation is deny-by-default and evaluates these grants:

- authenticated operator control;
- supervising parent;
- peer invocation of active persistent independent agents;
- explicitly authorized peer agents;
- external ingress bindings; and
- future scoped runtime capabilities.

Capability and message policy are not identity visibility.

The first migration may still use internal named packages. It must not preserve
`private_child` or `public_named` as the canonical policy model.

The initial send policy is:

| Caller evidence | Initial send decision |
|---|---|
| authenticated operator control with `agent.message.send` scope | allow unless an applicable explicit deny exists |
| active supervising parent using the supervision follow-up route | allow while that supervision remains active |
| lineage parent without active supervision | deny unless an explicit scoped allow exists |
| peer agent using the `agent_invocation` route to an active persistent independent target | allow unless an applicable explicit rule overrides it |
| any other peer-agent message | deny unless an explicit scoped allow exists |
| configured external ingress binding | allow only within that binding's target and content scope |
| any other caller | deny |

Lifecycle fences and authenticated namespace resolution run before policy
grants. Within message policy, an applicable explicit deny wins over derived or
explicit allow. A narrower principal, target, route, or ingress-binding rule
wins over a broader rule; equally scoped conflicting rules resolve to deny.
Operator control may change policy through a separate authorized management
operation, but message admission does not silently bypass an explicit deny.

Closing supervision removes its derived parent send grant immediately. It does
not erase lineage and it does not create a peer grant. Detaching lifecycle from
supervision likewise does not preserve the derived grant. External bindings
never inherit operator, lineage, or supervision authority.

## 3. Operator Tree And Independent Entry

Authenticated operator projections should expose active agents as a tree:

```text
root-agent
├── research-agent
├── implementation-agent
│   └── test-agent
└── review-agent
```

The tree is built from lineage records, not from a public/private filter.

Each visible node has an independent detail entry that can expose, subject to
existing retention and authorization rules:

- identity and lifecycle;
- parent and child lineage;
- active supervision;
- current scheduling posture;
- WorkItems and tasks;
- conversation/message history;
- artifacts and evidence;
- capability and message-policy summaries; and
- workspace projection.

The ordinary tree hides `Deleted` identities. Historical lineage, task
evidence, deletion jobs, and tombstones remain queryable through their
authorized historical surfaces.

Operator visibility does not imply peer visibility. The narrow peer invocation
grant is derived from canonical `persistent + independent` lifecycle facts, not
from tree visibility, and does not grant peer enumeration or other message
routes.

## 4. Operator Interaction With Subagents

An authenticated operator may send a direct message to an active subagent.

That message is admitted as operator-origin input. It must not be relabeled as:

- a parent follow-up;
- a delegated-task continuation; or
- a child task result.

The envelope may include an explicit, validated correlation to an
`ActorInvocation` task, WorkItem, or prior delivery. Correlation does not change origin or
authority.

A direct operator message does not complete, replace, or implicitly reopen the
parent's invocation task. Any effect on delegated acceptance remains explicit
in the task protocol.

If a supervision-attached ephemeral child has entered deletion, cleanup, or
tombstoned state, a direct message is rejected by the same lifecycle fence as
all other ingress.

## 5. Agent-Facing Operations And Internal Delivery

### 5.1 `CreateAgent`

`CreateAgent` creates an independently managed agent through the canonical
create service.

Default relation values:

```text
lineage_parent_agent_id = null
supervision = none
durability = persistent
lifecycle_attachment = independent
```

Creation and first initialization remain separate internal steps. The canonical
create receipt and post-commit bootstrap/repair contract remain authoritative.

An optional initial message is part of the create request for a new identity.
It is bootstrap input, does not create a result-bearing task, and must never
turn an already-existing identity into a message target. A caller that needs a
terminal brief uses `InvokeAgent`.

### 5.2 `InvokeAgent`

`InvokeAgent` creates one result-bearing `ActorInvocation` task. It is
asynchronous: the operation returns a `TaskHandle` immediately, and callers use
the normal `WaitFor(task_result)` contract when they need the terminal brief.

The target is an explicit discriminated union:

```text
existing_agent
  agent_id

new_subagent
  create_spec
```

For `existing_agent`, invocation does not create, reconfigure, reparent,
detach, or acquire lifecycle authority over the target.

For `new_subagent`, the same application-level operation:

- creates one agent identity;
- establishes parent-child lineage;
- establishes parent-owned lifecycle supervision;
- applies the default ephemeral, supervision-attached lifecycle policy; and
- admits the initial invocation message.

The operation returns the target `agent_id`, whether it was created by this
request, and the invocation `TaskHandle`. Delegation remains bounded.
Workspace/worktree selection remains an execution projection property governed
by the delegation contract.

The parent may later invoke the same child through `existing_agent`. Completing
an invocation closes only that task; it does not close supervision or delete
the child.

### 5.3 Internal Agent Message Delivery

`AgentMessageDeliveryService` targets an existing agent identity. It does not
create, reconfigure, reparent, detach, or change the model of that agent.

The internal operation returns a delivery receipt, not a task result. It is
used by `InvokeAgent`, operator ingress, and runtime adapters that have an
explicitly authorized route.

If a caller needs a result-bearing invocation, a higher-level facade may:

1. create an invocation task or WorkItem correlation;
2. admit one message through the delivery service; and
3. wait through the normal task/WorkItem continuation contract.

That facade must not change the message admission semantics defined here.

A general agent-facing `SendAgentMessage` tool is deferred until a concrete
fire-and-forget use case justifies a second public messaging entry point.

## 6. Message Delivery Request

The core request is:

```text
AgentMessageSendRequest
  target_agent_id
  content
  client_idempotency_key
  correlation?
  requested_priority?
```

Trusted runtime context supplies:

```text
AgentMessageCallerContext
  caller_principal
  caller_agent_id?
  origin
  delivery_surface?
  admission_context?
  authority_class
  trust?                 # compatibility projection only
  ingress_binding?
  current_turn_id?
  current_task_id?
  current_work_item_id?
```

Caller-controlled fields must not be able to override trusted caller context.
In particular, a sender cannot claim operator origin, copy a stronger authority
class, or manufacture parent-supervision provenance.

The admitted target envelope durably preserves:

```text
AgentMessageEnvelope
  delivery_id
  caller_principal
  caller_agent_id?
  target_agent_id
  origin
  delivery_surface?
  admission_context?
  authority_class
  trust?                 # compatibility projection only
  correlation_id?
  causation_id?
  admission_evidence
```

`admission_evidence` identifies the policy grant or active relation used for
admission and the lifecycle fence observed by the transaction. The immutable
origin, authority class, correlation, causation, and admission evidence enter
message admission, transcript storage, target prompt context, and restricted
audit evidence. Compatibility `trust` must not replace `authority_class`.

Admission authority does not become execution authority. The target runtime
computes execution permission from the preserved envelope, the target agent's
capability policy, and the current execution policy. A message admitted from a
supervisor, peer, or external binding cannot make later target tool calls run
with operator authority.

`requested_priority` is only accepted where the caller is authorized and the
existing scheduler contract can represent it. It is not a general priority
escalation field.

## 7. Admission Order And Information Hiding

The service evaluates one request in this order:

1. authenticate or bind trusted caller context;
2. resolve the target within the caller's authorized namespace;
3. validate content, correlation, and idempotency key;
4. begin the atomic admission transaction and read and fence the target
   identity lifecycle;
5. authorize message admission under relation and message policy from the same
   transaction snapshot, recording any derived grant in admission evidence;
6. atomically create or reuse the delivery and queue record;
7. commit the admission transaction; and
8. trigger post-commit scheduler reconciliation.

The lifecycle fence is therefore established before any policy grant is
accepted and remains part of the same transaction that records admission.
Relation closure, lifecycle changes, or policy changes that conflict with the
transaction must force retry or rejection rather than admit against a stale
grant.

Unauthorized callers must not gain an agent enumeration oracle. Adapters may
map unauthorized and unknown targets to the same external `not_found` response
while preserving the internal rejection reason in restricted audit evidence.

## 8. Delivery Receipt And State

The durable receipt is:

```text
AgentMessageDeliveryReceipt
  delivery_id
  target_agent_id
  outcome
  state
  accepted_at?
  terminal_at?
  idempotent_replay
  rejection_code?
  retryable
  lifecycle_snapshot
  correlation?
```

Initial outcomes:

```text
accepted
rejected
```

Initial durable states:

```text
queued
dispatched
consumed
failed
cancelled_by_deletion
rejected
```

`accepted` means one durable queue admission exists. It does not promise:

- immediate wake;
- immediate turn execution;
- exactly-once provider execution;
- successful model output; or
- a result to the sender.

`consumed` means that the admitted message was incorporated into a target turn.
It does not mean that requested work completed successfully.

Adapters may initially return only `queued` or `rejected`, while later
observation surfaces expose subsequent state.

Receipts do not include full provider traces or unrestricted message content.

The initial state machine is:

| Current state | Allowed next state | Meaning |
|---|---|---|
| no record | `queued` | admission and queue insertion committed |
| no record | `rejected` | a non-retryable rejection was terminally recorded |
| `queued` | `dispatched` | target execution claimed or observed the message |
| `queued` | `failed` | accepted delivery cannot be dispatched or recovered |
| `queued` | `cancelled_by_deletion` | deletion cutoff cancelled unconsumed work |
| `dispatched` | `consumed` | a target turn incorporated the message |
| `dispatched` | `failed` | target incorporation terminated with durable diagnostics |
| `dispatched` | `cancelled_by_deletion` | deletion won before turn incorporation committed |

`consumed`, `failed`, `cancelled_by_deletion`, and `rejected` are terminal.
States never regress. A dispatch lease expiry or restart may reissue dispatch
while retaining `dispatched`; it must not move the record back to `queued`.
Every transition uses a compare-and-set state/version check so deletion,
dispatch, and consumption have one observable winner.

Deletion racing with `dispatched` linearizes against target turn incorporation.
If incorporation commits first, the delivery is `consumed` and deletion
continues without rewriting it. If the deletion cutoff commits first, later
incorporation is fenced and the delivery becomes `cancelled_by_deletion`.

## 9. Idempotency

Idempotency is scoped to the authenticated caller principal, target agent, and
operation namespace.

For an accepted delivery or a non-retryable rejection, the service stores a
terminal idempotency record containing:

- a digest of the normalized semantic request;
- the client idempotency key;
- the resolved target `agent_id`; and
- the resulting receipt.

Repeating the same key and semantic request returns the same receipt with
`idempotent_replay = true`.

Reusing the key with different content, target, correlation, or other semantic
fields returns `idempotency_conflict`.

Retryable pre-admission rejections do not terminalize the idempotency key.
`agent_stopped` and `queue_unavailable` may be recorded as immutable attempt
audit evidence, but a later request with the same key and semantic request
re-evaluates policy, lifecycle, and queue availability. It may therefore
succeed after an authorized start or queue recovery.

The admission transaction first checks the unique idempotency scope before
inserting a delivery. If an earlier attempt committed admission but its
response was lost, retrying the same key returns that accepted delivery. If no
admission committed, the same key can be evaluated again without risking a
second queue record. Retryable rejection receipts set
`idempotent_replay = false`; only a stored terminal record is replayed.

Idempotency prevents duplicate queue admission. Post-commit wake and scheduler
reconciliation may be retried. Provider turns and external side effects remain
governed by their own delivery and execution contracts.

The first implementation adds no global ordering promise, message expiry, or
dead-letter queue. Per-target queue order follows the canonical runtime ledger
sequence. If capacity or durable queue admission is unavailable, the request is
rejected with a retryable `queue_unavailable` result rather than accepted into
an untracked backlog.

## 10. Lifecycle Fence

Message admission linearizes in the same database transaction that validates
the target identity lifecycle and inserts the delivery/queue record.

### Active And Running

Accept when policy permits.

### Administratively Stopped

Reject with `agent_stopped`.

The first implementation must not persist a hidden backlog and must not
implicitly start or wake a stopped agent. The rejection is retryable after an
authorized start.

### Deleting

Reject with `agent_deleting`.

Deletion is irreversible, so the same identity is not a retryable target.

### Deleted Or Tombstoned

Reject with `agent_deleted` on authorized historical/control surfaces.
Ordinary or information-hiding adapters may expose the established gone/not
found mapping.

The technical agent ID is not recreated or reused.

### Delivery Accepted Before The Delete Fence

The delete transaction establishes the cutoff.

- A delivery that commits after the fence is rejected.
- A queued delivery that committed before the fence must not reactivate the
  agent after the fence.
- Deletion cleanup transitions undispatched accepted deliveries to
  `cancelled_by_deletion`.
- Already-running work follows canonical stop/delete convergence and cannot
  enqueue a new post-fence activation.

Post-commit wake is therefore a retryable hint, not the admission linearization
point.

## 11. Typed Rejections

The initial internal rejection taxonomy includes:

```text
target_not_found
message_not_authorized
invalid_message
invalid_correlation
invalid_priority
idempotency_conflict
agent_stopped
agent_deleting
agent_deleted
queue_unavailable
```

Each adapter maps these to its protocol while preserving:

- stable machine-readable code;
- retryability;
- a bounded operator-facing message; and
- restricted audit detail.

Provider or scheduler failure after durable acceptance does not retroactively
change the admission outcome to rejected. It advances the delivery state.

## 12. Audit Evidence

Durable delivery evidence records:

- delivery ID and target agent ID;
- caller principal and caller agent ID when present;
- immutable origin, trust, and authority;
- relation used for authorization;
- idempotency-key digest and request digest;
- correlation and causation references;
- lifecycle version or equivalent fence evidence;
- acceptance/rejection decision;
- queue/message reference;
- subsequent delivery state transitions; and
- bounded failure/rejection diagnostics.

Message content follows existing retention, redaction, and access policy. Audit
requirements do not justify copying secrets into unrestricted logs.

## 13. Adapter Boundary

The application services are canonical:

```text
AgentCreationService
AgentInvocationService
AgentMessageDeliveryService
```

Adapters perform schema decoding, authentication/context binding, result
mapping, and presentation only.

### Native Tools

Keep a small typed native tool kernel for operations that depend on current
turn provenance, task supervision, yield/rejoin, workspace projection,
lifecycle fences, or scoped authority.

First-release agent-facing tools:

```text
CreateAgent
InvokeAgent
```

Native tool names use action-first PascalCase: `VerbObject`, with additional
object qualification only when needed to distinguish the operation. Service
and domain type names may remain noun-oriented and must not define a second
tool naming convention.

`InvokeAgent` remains asynchronous despite the result-bearing name: it returns
a `TaskHandle`, not the terminal result.

### CLI

The CLI remains appropriate for operator administration, scripting, and
read-only control-plane queries.

Agent-mode CLI declarations such as inherited caller environment fields are
provenance declarations, not authentication capabilities. Cross-agent writes
must not be enabled through agent-mode CLI until the runtime can issue and
verify an unforgeable, scoped capability.

### HTTP

The first implementation does not add authenticated operator HTTP invoke,
general direct-message, or delivery-receipt surfaces. They may be added later
when a concrete operator client requires them. Agent-facing HTTP would require
the same scoped authorization model as native tools.

### Future Protocol Adapters

MCP, ACP, A2A, or other adapters may project these services. They do not define
a second lifecycle or delivery state machine.

## 14. `SpawnAgent` Retirement

`SpawnAgent` does not receive a versioned agent-facing compatibility window.
When `CreateAgent` and `InvokeAgent` are exposed, `SpawnAgent` is removed from
the native tool registry, schemas, provider exposure, prompt guidance, and
templates.

Migration of current callers follows this semantic mapping:

```text
preset = public_named
  -> CreateAgent

preset = private_child
  -> InvokeAgent(target = new_subagent)
```

During implementation phases before replacement tools are exposed, the current
code path may be routed through canonical application services to avoid a
parallel state machine. That internal transition is not a published
compatibility commitment or a deprecated tool alias.

## 15. Legacy Data Migration

Migration is additive and staged.

### Stage 1: Semantic Freeze

- mark `public/private`, `public_named/private_child`, and combined
  `SpawnAgent` semantics deprecated;
- stop adding new product behavior that depends on those labels; and
- make this RFC the target contract.

### Stage 2: Additive Canonical Records

Add canonical lineage, supervision, durability, lifecycle attachment,
capability policy, and message policy records without removing legacy fields.

All new application services write canonical fields. Readers prefer canonical
fields and fall back through one compatibility mapper.

### Stage 3: Evidence-Based Backfill

Backfill from durable evidence:

- existing parent and lineage IDs;
- delegated supervision task IDs;
- task/result routing;
- ownership and cleanup evidence;
- capability-family configuration; and
- current identity lifecycle.

Do not infer the entire new model only from a preset string. Ambiguous records
are reported and handled explicitly.

Required preservation:

- technical agent ID;
- AgentHome association;
- message and turn history;
- lineage;
- task handles and result rejoin;
- workspace/worktree ownership evidence;
- lifecycle/deletion jobs; and
- tombstones.

Migration must not recreate agents.

### Stage 4: Stop Legacy Writes

Remove legacy parameters from current-version OpenAPI, CLI, native tools, and
templates. The current native tool surface removes `SpawnAgent`; no versioned
agent-facing adapter is required.

### Stage 5: Remove Legacy Model

After backfill, rollback, and migration evidence requirements are satisfied:

- remove identity use of `AgentVisibility`;
- replace bundled `AgentOwnership` inference with lifecycle attachment and
  supervision;
- remove `AgentProfilePreset::{PrivateChild, PublicNamed}`;
- remove `AgentKind::Child` as an authoritative stored classification if
  lineage can derive it safely; and
- remove unreachable `SpawnAgent` implementation residue.

## 16. Compatibility Mapping

The initial mapper uses all available evidence, with these defaults only when
the record is unambiguous:

| Legacy evidence | Canonical projection |
|---|---|
| `public_named`, self-owned, no supervision task | persistent, independent |
| `private_child`, parent ID, active supervision task | ephemeral, supervision-attached |
| `AgentVisibility::Private` | no operator-tree hiding; map peer access through message policy |
| `AgentOwnership::ParentSupervised` | active supervision plus supervision-attached lifecycle |
| `AgentOwnership::SelfOwned` | independent lifecycle unless stronger evidence exists |
| `AgentKind::Child` | derive lineage child relation from parent evidence |

Defaults are not permission grants. Peer message access remains deny-by-default
unless a policy or relation authorizes it.

## 17. Implementation Sequence

Implementation should be split into independently reviewable changes.

### Phase A: Contract Types And Compatibility Projection

- add canonical relation/policy types and schema;
- add legacy-to-canonical read projection;
- preserve existing write behavior;
- add round-trip and ambiguity tests.

No tool rename, queue change, or data deletion belongs in this phase.

### Phase B: Agent Tree And Detail Projection

- expose lineage parent/children in canonical detail;
- build authenticated operator tree projection;
- allow independent navigation to supervised children;
- keep peer enumeration unchanged.

No message-send capability belongs in this phase.

### Phase C: Split Create And Invocation Services

- retain the canonical create transaction;
- extract one `AgentInvocationService`;
- add canonical create/invoke request and result types;
- route the current `SpawnAgent` implementation through the canonical services
  as a temporary internal transition.

No cross-agent message delivery belongs in this phase.

### Phase D: Delivery Repository And Lifecycle Fence

- add delivery records and state transitions;
- implement idempotency;
- atomically admit queue messages against lifecycle state;
- reconcile wake after commit;
- integrate delete cutoff and cancellation;
- persist transition diagnostics and expose an internal receipt query by
  delivery ID or idempotency scope.

This phase may use internal test adapters. It does not expose unauthenticated or
agent-mode CLI writes.

### Phase E: Native Tool And Operator Adapters

- add typed `CreateAgent`;
- add typed `InvokeAgent`;
- remove `SpawnAgent` from the agent-facing registry, schemas, provider
  exposure, prompt guidance, and templates;
- implement the initial deny-by-default message authorization matrix;
- enforce information-hiding error mapping;
- preserve current-turn provenance in native tools;
- keep delivery-receipt observation internal; and
- defer authenticated operator HTTP invoke and general direct-message
  surfaces.

### Phase F: Canonical Writes And Data Backfill

- switch create/invoke paths to canonical fields;
- run restart-safe, idempotent backfill;
- expose migration diagnostics;
- verify identity/history/lineage/tombstone preservation.

### Phase G: Legacy Surface Removal

- stop legacy writes;
- remove preset parameters from current contracts;
- remove obsolete enums, branches, and unreachable legacy code after backfill
  evidence and rollback requirements are satisfied.

Each phase requires its own review and verification. Removing legacy state must
not be combined with first introducing the replacement state.

## 18. Acceptance Matrix

### Identity And Projection

- every active supervised child appears under its lineage parent;
- operator navigation reaches the child's independent detail and conversation;
- peer agents cannot enumerate the operator tree without explicit authority;
- when detach is implemented, detaching lifecycle does not erase lineage;
- deleted identities stay out of the ordinary tree but retain historical
  evidence.

### Create And Invocation

- independent create returns an identity/creation receipt and no supervision
  task;
- invocation of an existing agent returns its identity and an actor-invocation
  task without changing lineage or lifecycle ownership;
- invocation with `new_subagent` returns a child identity and actor-invocation
  task;
- duplicate create does not send a message or overwrite configuration;
- delegated result rejoin remains intact;
- invocation completion does not delete the target agent;
- the parent can invoke the same supervised child again; and
- first-release parent-selected supervised cleanup supports keep/reuse, stop,
  and the canonical deletion lifecycle.

### Message Delivery

- authorized active delivery creates exactly one queue admission;
- an idempotent replay returns the same receipt;
- key reuse with different semantics is rejected;
- retryable stopped/queue-unavailable rejection can succeed later with the same
  key, while a response-lost committed admission still replays one delivery;
- authenticated operator control, active supervision, lineage-only parent,
  persistent independent peer invocation, explicitly authorized peer, external
  binding, and unknown caller follow the initial authorization matrix;
- an applicable explicit deny overrides an allow and supervision close removes
  the derived parent grant; when detach is implemented, lifecycle detach does
  not recreate it;
- operator direct message and parent follow-up retain different provenance;
- target transcript and prompt context preserve origin, authority class,
  correlation, causation, and admission evidence;
- unauthorized/unknown mapping does not leak target existence;
- stopped targets reject without hidden queueing or wake;
- deleting/deleted targets reject;
- delete racing with send has one observable linearization order;
- pre-fence queued messages cannot reactivate a deleting agent;
- `queued`, `dispatched`, and each terminal transition follow the declared
  state table and cannot regress;
- delete racing with dispatched incorporation yields exactly one of `consumed`
  or `cancelled_by_deletion`;
- accepted failures retain durable bounded diagnostics and never become
  `rejected`;
- authorized receipt observation returns the latest durable state;
- restart recovery preserves receipt, diagnostics, and queue consistency.

### Migration

- no agent ID changes;
- no message, turn, task-result, or lineage history is recreated;
- no tombstone is removed or made reusable;
- ambiguous legacy records are reported rather than guessed;
- canonical readers work during mixed-version data;
- legacy writes stop before legacy fields are removed.

## 19. Open Questions

The following can be resolved during implementation review without reopening
the core identity decision:

1. whether the first internal receipt-query surface exposes `consumed` and
   `failed`, or only admission, consumption, and cancellation state.

The following are not open:

- subagents are first-class addressable agents;
- operator tree visibility is separate from peer authorization;
- direct operator messages preserve operator provenance;
- delegated children remain ephemeral and supervision-attached by default;
- stopping an attached child preserves parent lifecycle ownership and the
  cleanup obligation until deletion or a future explicit detach/persist
  transition;
- invoking an existing agent does not acquire lifecycle authority;
- invocation completion never implicitly stops or deletes its target;
- the supervising parent chooses the subagent lifecycle action and the runtime
  durably executes it;
- canonical relation and policy state uses normalized records;
- the first release supports reuse, stop, and delete but defers explicit
  detach/persist;
- the first native tool surface contains only `CreateAgent` and `InvokeAgent`;
- `SpawnAgent` has no versioned agent-facing compatibility window;
- authenticated operator HTTP invoke is deferred;
- create, invocation, and internal message delivery are separate state
  machines; and
- delivery acceptance is asynchronous and lifecycle-fenced.

## Decision

Adopt one agent identity model, express subagent behavior through explicit
relations, and deprecate private/public agent classification.

Introduce a dedicated asynchronous message delivery service with idempotent
receipts and a deletion-aware lifecycle fence, but do not expose a general
`SendAgentMessage` native tool in the first release.

Expose `CreateAgent` and `InvokeAgent`, remove `SpawnAgent` from the
agent-facing tool surface without a versioned compatibility window, and defer
explicit detach/persist plus authenticated operator HTTP invoke.
