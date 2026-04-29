# Holon RFCs

This directory holds the current RFC set for Holon's long-lived runtime and
public contract.

For a short architecture map and reading path, see
[Holon Architecture Overview](../architecture-overview.md).

## Runtime And Work Model

- [Agent Control Plane Model](./agent-control-plane-model.md)
- [Agent Profile Model](./agent-profile-model.md)
- [Result Closure](./result-closure.md)
- [Continuation Trigger](./continuation-trigger.md)
- [Objective, Delta, and Acceptance Boundary](./objective-delta-and-acceptance-boundary.md)
- [Work Item Runtime Model](./work-item-runtime-model.md)
- [Long-Lived Context Memory](./long-lived-context-memory.md)
- [Agent and Workspace Memory](./agent-and-workspace-memory.md)
- [Turn-Local Context Compaction](./turn-local-context-compaction.md)
- [Operator Notification and Intervention](./operator-wait-and-intervention.md)
- [Waiting Plane And Reactivation](./waiting-plane-and-reactivation.md)
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
