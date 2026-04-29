---
title: RFC: Command Tool Family
date: 2026-04-21
status: draft
---

# RFC: Command Tool Family

## Summary

This RFC defines Holon's command tool family as one coherent public surface.

The command family is responsible for:

- starting shell execution
- managing long-running command lifecycle
- inspecting command-backed runtime state
- retrieving command output
- stopping command execution
- later supporting interactive command continuation

The intended public family center is:

- `ExecCommand`
- `TaskList`
- `TaskStatus`
- `TaskOutput`
- `TaskInput`
- `TaskStop`

Command-backed execution is the primary center of the task surface, but Holon
may still expose other managed execution jobs through the same task
inspection/control family when they share the same operational semantics.

## Problem

Holon already has substantial command-related capability, but it is still
described across several overlapping documents and surfaces:

- `ExecCommand` starts shell execution
- long-running non-interactive commands become `command_task`
- `TaskList` / `TaskStatus` / `TaskOutput` / `TaskStop` inspect and control
  runtime task state
- interactive command continuation has its own pending design

This creates three problems.

First, the family boundary is still implicit.

The runtime already behaves as if there is a command family, but the public
contract is still partly expressed as:

- one command tool
- plus some generic task tools

Second, command lifecycle is still split across multiple documents:

- `docs/command-execution-and-task-model.md`
- `docs/rfcs/interactive-command-continuation.md`
- `docs/rfcs/task-surface-narrowing.md`

Those documents are all useful, but none of them alone defines the command
family as one public tool family.

## Goals

- define the command family as one explicit public tool family
- clarify which tools belong to that family
- clarify the boundary between command lifecycle and generic task language
- make room for future interactive continuation without changing the family
  identity again

## Non-goals

- do not fully specify PTY substrate implementation details
- do not replace the runtime task model in this RFC
- do not force every runtime-owned job into the command family

## Family Boundary

The command family answers these questions:

- what shell command should run now?
- what managed command execution is still running?
- how do I inspect its metadata?
- how do I read its output?
- how do I stop it?
- how do I continue it if it becomes interactive?

The command family does not answer:

- what high-level work item is the agent pursuing?
- whether another child context should be spawned
- what future external condition should wake the runtime

That means the command family belongs to the command plane and should stay
separate from:

- work plane
- agent plane
- waiting plane

## Command Family Members

## 1. `ExecCommand`

`ExecCommand` is the command startup primitive.

It should be the only public entry point for starting shell execution in the
command family.

Its intended use cases are:

- diagnostics
- verification
- repository inspection through shell-first patterns
- short one-shot command execution
- startup of longer command execution that may later be managed by the runtime

The command family should not grow a second public startup primitive such as:

- `RunCommand`
- `StartCommandTask`
- `background=true` command variants

The startup path should remain singular.

## 2. `TaskList`

Within the command family, `TaskList` should primarily answer:

- which managed command executions are currently relevant?

Even if Holon still allows other runtime jobs to appear internally, the model
guidance should increasingly teach `TaskList` as the compact overview of
command-backed managed execution.

Its default return shape should stay compact.

The same family may still expose other managed execution jobs when they share
the same task-like operational semantics:

- lifecycle state
- inspectable metadata
- stoppable execution
- retrievable output or result

## 3. `TaskStatus`

`TaskStatus` should provide:

- one task handle's current lifecycle snapshot
- family-specific metadata such as command summary, continuation-relevant
  flags, and output availability

For the command family, `TaskStatus` is the metadata inspection path rather than
the raw-output path.

## 4. `TaskOutput`

`TaskOutput` should be the heavyweight output path for managed command
execution.

Its role in the command family is:

- wait for completion when appropriate
- retrieve stable output content
- surface command-family output in a form suitable for later reasoning

For the command family, this is the main content path after startup.

Command output should still use Holon's shared tool result envelope. The command
family owns only the inner `result` payload, including command disposition,
exit status, bounded output previews, task handles, and artifact references.

## 5. `TaskStop`

`TaskStop` should be the stop primitive for managed command execution.

The command-family meaning should be:

- request termination of the managed command lifecycle
- receive stable state transition semantics
- avoid treating stop as a fragile convergence barrier

This means command-family stop semantics should remain bounded and observable,
not overly synchronous.

## 6. `TaskInput`

`TaskInput` is the input-side continuation primitive for managed execution.

In the first version, it should:

- send structured text input only to managed `command_task` instances that were
  explicitly created with stdin continuation enabled
- return a stable continuation receipt rather than a bare boolean
- report non-fatal rejection as structured output with `accepted_input = false`
  and a compact reason instead of collapsing every refusal into a transport
  error
- remain generic enough to cover both command stdin / tty continuation and
  parent-supervised child follow-up on the same surface

## 7. Continuation Direction

If Holon expands interactive command continuation further, those tools should
still belong to the command family.

Examples of command-family continuation capability beyond `TaskInput`:

- richer incremental output retrieval for managed command execution

The exact names remain open, but the family boundary is not open:

- startup
- inspection
- output
- stop
- continuation

all belong to the command family.

## Internal Runtime Relationship

The command family is a public family. It is not identical to one internal type.

Internally, Holon may still represent long-running command execution through
`command_task` or a later renamed runtime job type.

What matters at the public layer is:

- `ExecCommand` starts command execution
- long-running command execution becomes managed runtime state
- task-oriented tools inspect and control that state

This RFC therefore does not require Holon to collapse all runtime job types
into one command-only internal abstraction.

It also does not require the public task surface to be command-exclusive.
Instead, the intended rule is:

- command-backed execution is the primary center of the task surface
- other managed execution jobs may still appear in the same inspection/control
  family when they share the same operational semantics
- agent delegation and waiting should not remain the primary long-term meaning
  of that task surface

## Naming Direction

The command family should follow Holon-native built-in naming conventions.

That means:

- PascalCase built-in tool names
- no namespace prefix

So the intended family spelling is:

- `ExecCommand`
- `TaskList`
- `TaskStatus`
- `TaskOutput`
- `TaskInput`
- `TaskStop`

## Relationship To Existing Notes And RFCs

This RFC is the family-level umbrella for command tools.

Related documents have narrower roles:

- `docs/command-execution-and-task-model.md`
  - detailed model for command startup, promotion, and runtime ownership
- `docs/rfcs/interactive-command-continuation.md`
  - future interactive continuation design
- `docs/rfcs/task-surface-narrowing.md`
  - broader migration of public task semantics toward command-centered usage
- `docs/rfcs/tool-contract-consistency.md`
  - naming, schema, and output consistency rules across all tool families
- `docs/rfcs/tool-result-envelope.md`
  - shared model-visible success/error envelope used by command-family tools

This RFC does not replace those documents. It gives them one explicit public
family container.

## Migration Direction

The intended migration path is:

1. define the command family explicitly
2. make prompt guidance teach the task tools as the managed command lifecycle
   surface
3. keep delegation and waiting out of the command-family creation surface
4. later add interactive continuation without changing the family boundary

## Open Questions

The following questions remain open after this RFC:

- should non-command runtime jobs still appear in `TaskList` for model-facing
  inspection, or should that increasingly become an operator/debug concern?
- should command-family `result` payloads need a deeper typed schema than the
  shared outer tool result envelope?

## Summary

Holon should define one explicit command tool family.

Its center should be:

- `ExecCommand`
- `TaskList`
- `TaskStatus`
- `TaskOutput`
- `TaskInput`
- `TaskStop`

This makes command execution, managed lifecycle, and future interactive
continuation part of one coherent public contract instead of several loosely
related surfaces.
