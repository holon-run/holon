---
title: RFC: Agent-Aware CLI Invocation Context
date: 2026-09-02
status: draft
---

# RFC: Agent-Aware CLI Invocation Context

## Summary

Holon agents may invoke the local `holon` CLI from a command task. The CLI
needs to preserve who is making the call and where the call came from without
turning shell-provided metadata into an authentication mechanism.

This RFC defines a small, declaration-based invocation context:

```text
caller_agent_id
source_task_id
source_turn_id
source_work_item_id?
source_activation_id?
inherited_authority_class
```

The runtime supplies this context to agent-owned command tasks. The CLI parses
it and forwards the claims to the control plane as request provenance. A CLI
started without agent context retains the existing operator-mode behavior.

The context is a **provenance and behavioral contract**, not a proof of
identity. A local process can alter its environment or construct a request
that claims to be another caller. Control authentication remains the bearer
control token, and admission policy remains the authority boundary.

## Goals

- identify the declared agent caller and target separately;
- preserve source task, turn, work item, and activation lineage where present;
- inherit the source activation's effective authority in agent mode;
- keep ordinary, context-free CLI usage in operator mode;
- provide stable machine-readable context and command-discovery surfaces;
- make malformed explicit context fail with a structured diagnostic;
- make the same provenance shape available to existing control mutations.

## Non-goals

- preventing a local CLI process from pretending to be an agent or operator;
- introducing `HOLON_CALLER_CONTEXT_TOKEN` or another caller bearer capability;
- replacing the existing control bearer token;
- adding a new authority class such as `AgentInstruction`;
- expanding the first release to unrestricted recursive `run` or `prompt`;
- redesigning the existing task or WorkItem lifecycle API.

## Invocation modes

The CLI has two semantic modes:

1. **Operator mode**: no agent context is present. The caller and origin use
   the existing operator semantics.
2. **Agent mode**: a complete declared context is present. The caller is the
   declared agent, the target is selected independently, and authority is
   inherited from the source activation.

An explicit but malformed or incomplete context is a usage/control error. It
must not silently fall back to operator mode, because doing so would hide an
agent integration defect.

`HOLON_AGENT_ID` may continue to select an agent for existing commands where
that behavior is already documented, but it is not caller authentication and
must not override the declared caller context.

## Target semantics

Existing commands that accept an agent target use these rules:

- omitting `--agent` means the current caller's self target in agent mode;
- omitting `--agent` retains the existing operator default in operator mode;
- `--agent B` selects target `B` and does not change the caller;
- self and cross-agent operations are evaluated by the same admission policy.

The initial CLI surface uses `holon context` for the current non-sensitive
claims and `holon commands` for machine-readable command discovery. It does
not add a separate `holon self` namespace.

## Authority and origin

In agent mode, the request's effective authority is inherited from the source
activation. CLI flags and request bodies cannot raise it. A conflicting
authority value is rejected or ignored with an auditable diagnostic; it is
never treated as an upgrade.

The request retains its original `origin` and `root_origin`. A call that
originated from an operator remains operator-originated after an agent
delegates it, while an external, runtime, or integration-originated call
cannot become an `OperatorInstruction` merely because a CLI field says so.

The first implementation does not add a new `AgentInstruction` authority
variant. A need for a distinct authority class requires a separate RFC.

## Transport and audit

The CLI forwards declared caller claims through the local control client to the
server admission layer. The server records caller, target, source task, source
turn, source work item, source activation, origin, delivery surface, and
effective authority when those values are available.

No context token, credential, or secret is emitted by `holon context`,
`holon commands`, stdout results, or audit projections. The context fields are
safe-to-display identifiers and provenance claims only.

Control bearer authentication is performed before mutation admission. Declared
context is interpreted after authentication and cannot replace that check.

## CLI output and errors

The agent-facing contract is:

- `--output auto|json|jsonl|text`, with `auto` selecting compact JSON for
  non-TTY and agent-mode invocations;
- stdout contains successful results only;
- stderr contains diagnostics only;
- exit code `0` means success, `1` means an operational or control failure,
  and `2` means CLI usage failure;
- structured errors contain stable `code`, human-readable `message`,
  `retryable`, `details`, and an optional `recovery_hint`;
- existing successful payloads remain compatible unless a command explicitly
  opts into a new envelope.

## Initial implementation scope

Phase 1 and Phase 2 cover:

1. command-task-scoped declared caller context and admission propagation;
2. `holon context`;
3. `holon commands`;
4. `holon task list`;
5. WorkItem `create`, `pick`, and `update`;
6. WorkItem `complete --report-file <path|->` using the existing completion
   report contract;
7. caller, authority, target, and malformed-context regression tests.

Agent lifecycle, model override, destructive workspace control, and unrestricted
recursive `holon run`/`holon prompt` remain outside this scope.

## Acceptance matrix

| Case | Required result |
| --- | --- |
| Context-free CLI | Existing operator mode |
| Complete context | Agent caller and inherited authority are preserved |
| Missing required context field | Structured error; no operator fallback |
| Self target | `caller_agent_id == target_agent_id` |
| Cross-agent target | Caller remains source agent; target is independent |
| Conflicting authority flag | No escalation; reject or auditable ignore |
| Forged environment value | Not treated as authenticated identity |
| External/runtime source | Cannot become operator authority via CLI fields |
| `holon context` | Non-sensitive claims only |
| `holon commands` | Stable, machine-readable command metadata |

## Compatibility

This RFC is additive. Existing operator CLI calls, control bearer
authentication, task handles, and WorkItem completion semantics remain the
baseline. The implementation should introduce context handling at shared
client/admission boundaries rather than duplicating policy in each subcommand.
