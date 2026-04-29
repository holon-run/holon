# Basic Tool Comparison

This document records the current editing surface after the tool-result render
split and the removal of the dedicated `Write` and `Edit` tools.

## Current Public Coding Surface

Holon's current built-in coding tools center on:

- state / coordination: `AgentGet`, `SpawnAgent`, task tools, work-item tools
- shell / verification: `ExecCommand`
- file mutation: `ApplyPatch`
- workspace control: `UseWorkspace`

Holon no longer exposes separate `Write` or `Edit` tools in the public builtin
surface.

## Editing Policy

`ApplyPatch` is the single file-mutation primitive.

Provider-facing invocation depends on transport capability:

- OpenAI-compatible runs prefer a freeform grammar surface where the patch body
  is emitted directly
- JSON/function fallback providers use exactly
  `{"patch":"--- a/path\\n+++ b/path\\n@@ ...\\n"}` and do not accept `input`

Use it for:

- new files via `--- /dev/null` and `+++ b/path`
- small local edits via focused unified diff hunks
- whole-file replacement when that is actually intended
- multi-hunk or multi-file refactors
- file moves and deletes

Do not treat shell rewrite tricks such as `sed -i` or ad-hoc redirection as the
default editing path. Use `ExecCommand` to inspect and verify, and use
`ApplyPatch` to mutate files.

## Why Holon Chose This Shape

This keeps the tool surface smaller and removes overlapping edit primitives that
the model had to choose between:

- `Write` encouraged whole-file rewrites
- `Edit` overlapped with small `ApplyPatch` updates
- `ApplyPatch` already covered add, update, delete, and move semantics

The result is one stable editing contract for prompts, tests, and runtime
policy.
