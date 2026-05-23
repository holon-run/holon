# Holon RFCs

This directory holds the current RFC set for Holon's long-lived runtime and
public contract.

For a short architecture map and reading path, see
[Holon Architecture Overview](../architecture-overview.md).

## Recommended reading path

New contributors should read RFCs in this order rather than randomly:

### 1. Product and runtime map

- [README.md](../../README.md) — project entry and install
- [Architecture overview](../architecture-overview.md) — runtime map
- [Runtime model](../../docs/website/concepts/runtime-model.md) — user-facing
  concept page

### 2. Agent, work, and scheduler core

- [Agent State Model And Runtime Projection](./agent-state-model.md)
- [Work Item Runtime Model](./work-item-runtime-model.md)
- [Work Item Centered Agent Runtime](./work-item-centered-agent-runtime.md)
- [Runtime Scheduler Contract](./runtime-scheduler-contract.md)
- [Scheduler Wait State And Recoverable Agent Continuation](./scheduler-wait-state.md)

### 3. Waiting, wake, and external events

- [Continuation Trigger](./continuation-trigger.md)
- [Waiting Plane And Reactivation](./waiting-plane-and-reactivation.md)
- [Operator Notification and Intervention](./operator-wait-and-intervention.md)
- [External Trigger Capability And Providerless Ingress](./external-trigger-capability.md)

### 4. Tools and delegation

- [Tool Surface Layering](./tool-surface-layering.md)
- [Command Tool Family](./command-tool-family.md)
- [Agent Delegation Tool Plane](./agent-delegation-tool-plane.md)

### 5. Workspace, execution, and trust

- [Workspace Binding And Execution Roots](./workspace-binding-and-execution-roots.md)
- [Execution Policy And Virtual Execution Boundary](./execution-policy-and-virtual-execution-boundary.md)
- [Default Trust Authorization And Access Control](./default-trust-auth-and-control.md)

### 6. Provider, configuration, and context

- [Runtime Configuration Surface](./runtime-configuration-surface.md)
- [Long-Lived Context Memory](./long-lived-context-memory.md)

## Runtime And Work Model

- [Agent Control Plane Model](./agent-control-plane-model.md)
- [Agent Lifecycle Control Posture](./agent-lifecycle-control-posture.md)
- [Agent State Model And Runtime Projection](./agent-state-model.md)
- [Agent Profile Model](./agent-profile-model.md)
- [Runtime Scheduler Contract](./runtime-scheduler-contract.md)
- [Result Closure](./result-closure.md)
- [Continuation Trigger](./continuation-trigger.md)
- [Objective, Delta, and Acceptance Boundary](./objective-delta-and-acceptance-boundary.md)
- [Work Item Runtime Model](./work-item-runtime-model.md)
- [Work Item Centered Agent Runtime](./work-item-centered-agent-runtime.md)
- [Long-Lived Context Memory](./long-lived-context-memory.md)
- [Agent and Workspace Memory](./agent-and-workspace-memory.md)
- [Turn-Local Context Compaction](./turn-local-context-compaction.md)
- [OpenAI Remote Compaction Boundary](./openai-remote-compaction.md)
- [Turn Model Lineage And Recovery](./turn-model-lineage-and-recovery.md)
- [Operator Interjection Safe Points](./operator-interjection-safe-points.md)
- [Operator Notification and Intervention](./operator-wait-and-intervention.md)
- [Waiting Plane And Reactivation](./waiting-plane-and-reactivation.md)
- [Scheduler Wait State And Recoverable Agent Continuation](./scheduler-wait-state.md)
- [External Trigger Capability And Providerless Ingress](./external-trigger-capability.md)

## Provenance, Policy, And Execution

- [Provenance, Admission, and Authority](./default-trust-auth-and-control.md)
- [Remote Operator Transport and Delivery](./remote-operator-transport-and-delivery.md)
- [Execution Policy and Virtual Execution Boundary](./execution-policy-and-virtual-execution-boundary.md)
- [Runtime Configuration Surface](./runtime-configuration-surface.md)
- [Extensible Model And Provider Configuration](./extensible-model-provider-configuration.md)

## Workspace And Instruction Surface

- [Agent Initialization and Template](./agent-initialization-and-template.md)
- [Agent Home Directory Layout](./agent-home-directory-layout.md)
- [Workspace Binding and Execution Roots](./workspace-binding-and-execution-roots.md)
- [Instruction Loading](./instruction-loading.md)
- [Agent Workspace Switching](./workspace-entry-and-projection.md)
- [Skill Discovery and Activation](./skill-discovery-and-activation.md)

## Eventing And Client Surface

- [Event Stream Interface Design](./event-stream-interface.md)
- [Operator Display Levels and Event Presentation](./operator-display-levels-and-event-presentation.md)

## Tools And Delegation

- [Tool Surface Layering](./tool-surface-layering.md)
- [Command Tool Family](./command-tool-family.md)
- [Interactive Command Continuation](./interactive-command-continuation.md)
- [Agent Delegation Tool Plane](./agent-delegation-tool-plane.md)
- [Task Surface Narrowing](./task-surface-narrowing.md)
- [Tool Contract Consistency](./tool-contract-consistency.md)
- [Tool Result Envelope](./tool-result-envelope.md)
- [ApplyPatch Unified Diff Contract](./apply-patch-unified-diff-contract.md)

## Notes

- RFCs in this directory are architectural proposals and contract documents,
  not implementation status reports.
- The tools RFC series is intentionally split into multiple focused documents
  so command execution, delegation, and contract quality can evolve
  independently.
- Cross-plane and shared object-model decisions should prefer an RFC in this
  directory over a top-level note in `docs/`.
