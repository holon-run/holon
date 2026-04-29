# Holon Architecture Overview

This document is a short entry point for Holon's current architecture.

Canonical contracts live in `docs/rfcs/`. This overview should not introduce
new normative behavior. If this document conflicts with an RFC, the RFC wins.

## One Sentence

Holon is an agent-first, headless, long-lived runtime where agents receive
events through queues, execute through explicit tool surfaces, preserve
provenance and authority labels, delegate bounded work to child agents, and can
be reactivated by external systems or operator transport.

## Current Architecture Themes

- **Agent-first runtime:** an agent is the durable execution subject; host and
  control surfaces route work to agents.
- **Queue-centered execution:** operator input, runtime events, tasks,
  external triggers, and continuation signals all enter through explicit
  runtime surfaces.
- **Explicit provenance:** messages preserve `origin`, `delivery_surface`,
  `admission_context`, and `authority_class`.
- **Separated tool planes:** command execution, delegation, waiting, workspace,
  and operator notification are distinct tool/runtime concerns.
- **Providerless integration:** Holon defines runtime protocols; external
  systems such as AgentInbox implement transport or trigger adapters.
- **Workspace projection:** workspace roots, execution roots, worktrees, and
  occupancy are runtime-managed projections rather than implicit shell state.
- **Long-lived context:** durable logs remain append-only while model-visible
  context is assembled from bounded, structured memory projections.

## Core Objects

### Host

The host owns the process-level runtime container:

- agent registry
- server/control surfaces
- routing to target agents
- runtime configuration
- local process lifecycle

Canonical RFCs:

- [Agent Control Plane Model](./rfcs/agent-control-plane-model.md)
- [Runtime Configuration Surface](./rfcs/runtime-configuration-surface.md)
- [Event Stream Interface Design](./rfcs/event-stream-interface.md)

### Agent

An agent owns a durable execution context:

- identity and profile
- queue and lifecycle state
- transcript, briefs, and memory
- workspace bindings
- tool surface derived from profile/runtime capability
- optional parent/supervision relationship

Canonical RFCs:

- [Agent Profile Model](./rfcs/agent-profile-model.md)
- [Agent Control Plane Model](./rfcs/agent-control-plane-model.md)
- [Long-Lived Context Memory](./rfcs/long-lived-context-memory.md)

### Message And Provenance

Messages carry runtime-facing labels that explain source, admission, and
instruction authority. These labels are recording, prompt, audit, and future
policy inputs. They should not be treated as a per-turn tool-filtering
mechanism.

Canonical RFCs:

- [Provenance, Admission, and Authority](./rfcs/default-trust-auth-and-control.md)
- [Continuation Trigger](./rfcs/continuation-trigger.md)
- [Result Closure](./rfcs/result-closure.md)

### Work And Delegation

Holon separates high-level work identity from delegated execution. Work items
track meaningful work; child agents and tasks perform bounded execution.

Canonical RFCs:

- [Work Item Runtime Model](./rfcs/work-item-runtime-model.md)
- [Agent Delegation Tool Plane](./rfcs/agent-delegation-tool-plane.md)
- [Task Surface Narrowing](./rfcs/task-surface-narrowing.md)

### Tools

Holon's model-facing tools are organized by capability planes rather than a
flat bag of unrelated operations.

Canonical RFCs:

- [Tool Surface Layering](./rfcs/tool-surface-layering.md)
- [Tool Contract Consistency](./rfcs/tool-contract-consistency.md)
- [Command Tool Family](./rfcs/command-tool-family.md)
- [Interactive Command Continuation](./rfcs/interactive-command-continuation.md)

### Workspace And Execution

Workspace state is explicit and inspectable. Agents should not depend on
ambient process cwd alone to define what they can see or mutate.

Canonical RFCs:

- [Workspace Binding and Execution Roots](./rfcs/workspace-binding-and-execution-roots.md)
- [Workspace Entry and Projection](./rfcs/workspace-entry-and-projection.md)
- [Execution Policy and Virtual Execution Boundary](./rfcs/execution-policy-and-virtual-execution-boundary.md)

### External Integration

Holon should keep provider-specific SDKs outside core. Core owns generic
runtime protocols; adapters implement provider behavior.

Canonical RFCs:

- [External Trigger Capability And Providerless Ingress](./rfcs/external-trigger-capability.md)
- [Remote Operator Transport and Delivery](./rfcs/remote-operator-transport-and-delivery.md)
- [Operator Notification and Intervention](./rfcs/operator-wait-and-intervention.md)
- [Waiting Plane And Reactivation](./rfcs/waiting-plane-and-reactivation.md)

## Active Supporting Docs

These documents remain useful but are not the normative architecture source:

- [Runtime Spec](./runtime-spec.md)
- [Next Phase Direction](./next-phase-direction.md)
- [Local Operator Troubleshooting](./local-operator-troubleshooting.md)
- [Documentation Cleanup Audit](./documentation-cleanup-audit.md)
- [Implementation Decisions](./implementation-decisions/README.md)

## Historical Context

Older top-level notes may still exist for research, planning, or migration
history. When they conflict with this overview or the RFCs, treat them as
historical unless explicitly marked as active.
