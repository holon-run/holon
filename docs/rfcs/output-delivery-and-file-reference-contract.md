---
title: RFC: Output Delivery and File Reference Contract
date: 2026-09-17
status: draft
issue:
  - 3030
  - 3031
  - 3032
  - 3033
---

# RFC: Output Delivery and File Reference Contract

## Summary

Holon should make an agent's final brief or assistant message useful without
requiring the operator to recover intermediate progress, inspect tool logs, or
open an attached file. File references are entry points to supporting artifacts,
not substitutes for the result, verification status, risks, or required
operator action.

The correct file reference depends on where the reference is written:

- project Markdown in the same physical execution root uses a path relative to
  the document containing the reference;
- local records that cross workspace or worktree roots use a confirmed
  execution-host absolute path;
- Holon brief and assistant Markdown use a confirmed execution-host absolute
  path for links and images;
- public or portable output prefers a portable relative path or a confirmed
  published URL and does not expose machine-specific paths by default.

Historical `workspace://` references remain supported by consumers, but they
are not the intended default for newly generated output after the rollout gate
in this RFC is satisfied.

## Status And Rollout

The delivery and location rules in this RFC are accepted for implementation,
but the default prompt switch is intentionally staged:

1. **Contract phase:** document the rules and require self-contained delivery.
2. **Consumer phase:** verify resolver, Explorer, provider, and Web GUI behavior
   for the new location forms.
3. **Prompt phase:** change built-in file-reference guidance only after the
   consumer acceptance matrix passes.

Issue #3032 delivered the shared resolver, batch resolution API, complete
Explorer location identity, and provider image reuse. Issue #3033 owns the
remaining general Markdown-consumer behavior. Until that consumer work passes
the acceptance matrix, built-in guidance may continue to emit the historical
form; this RFC must not be read as permission to switch the default early.

## Delivery Contract

### Final output is self-contained

The final operator-facing delivery:

- leads with the conclusion, outcome, or blocker;
- includes material verification status and failed or unverified checks;
- surfaces risks and required operator action when they affect the next step;
- scales detail to the task instead of enforcing a fixed template, JSON shape,
  or mechanical line count;
- does not rely on transient progress messages or tool output for information
  the operator must retain.

This applies to ordinary assistant messages, briefs, and WorkItem completion
reports. A completion report is still required to summarize the result even
when it also links to a plan, report, image, build artifact, or changed file.

### File entry points supplement the result

A file reference should be descriptive and should identify a confirmed
location. It must not:

- replace the result summary;
- imply that an unverified or incomplete deliverable is complete;
- invent a workspace ID, execution root ID, local path, or published URL;
- widen access or publish an artifact only to make a link possible.

If no usable entry point is confirmed, report the delivery location and the
access limitation in prose.

## Location Matrix

| Output surface | New reference rule | Required boundary |
| --- | --- | --- |
| Project Markdown whose document and target are in the same physical execution root | Use a path relative to the document's directory | The path is not relative to process `cwd`; `..` is valid only while the resolved target remains in the same physical root |
| Local record referring across workspace or worktree roots | Use the confirmed execution-host absolute path | A shared logical workspace ID does not make two execution roots the same location |
| Holon brief or assistant Markdown link/image | Use the confirmed execution-host absolute path as the Markdown target; `file://` is not required | Preserve the actual worktree location and do not make the model assemble workspace or root IDs |
| Public channel, shared document, or publishable project documentation | Prefer a portable relative path or a confirmed published URL | Do not disclose a machine-specific absolute path by default; if no portable entry point exists, state the limitation instead of manufacturing a URL |
| Historical stored output containing `workspace://` | Preserve consumer compatibility, including execution-root identity | Do not rewrite stored content in bulk and do not silently drop or replace a root selector |

The public or portable-output rule overrides the local absolute-path rule when
machine-specific location disclosure would make the artifact unsafe or
non-portable.

## Path Identity And Failure Semantics

For new references, a leading `/` means an execution-host absolute path. It
must not also mean “relative to the current workspace.” Consumers must
distinguish historical workspace-root-relative forms through explicit syntax or
versioned compatibility behavior rather than ambiguous fallback.

Location resolution follows these rules:

- canonical and worktree files with the same relative path are distinct;
- a removed execution root or missing file fails instead of falling back to a
  canonical file with the same name;
- an unknown, removed, or foreign execution root keeps the 404, 410, or 403
  semantics defined by the workspace file APIs;
- a path identifies a location, not a content snapshot, permanent identity,
  browser-local path, access credential, or authorization grant.

The first rollout targets Unix host paths. Windows path syntax and a new
line-number grammar are outside this RFC's initial scope.

## Markdown Encoding

Markdown links and images use a correctly escaped target. Consumers and
producers must keep these cases distinguishable:

- spaces and parentheses in file names;
- literal `#` and `%` characters;
- Unicode path segments;
- an actual Markdown fragment versus a `#` that belongs to the file name.

An entire path may be written as inline code when the goal is to communicate
the location as text. Inline code does not promise that every client will make
the path clickable.

The exact encoding and fragment behavior must be covered by the consumer tests
owned by #3032 and #3033 before the prompt phase is enabled.

## Relationship To Existing Workspace References

`workspace://<workspace_id>/<path>?root=<execution_root_id>` remains a valid
Holon-owned locator. Its execution-root selector is opaque and must be preserved
by consumers. The runtime may continue to read historical output that uses this
form.

New output should not require a model to discover or concatenate workspace and
execution-root IDs merely to link a known file. The shared resolver described
by [Workspace File Browsing API](./workspace-file-browsing-api.md) and the
registry described by
[Execution Root Registry](./workspace-execution-root-registry.md) own that
translation and authorization boundary.

This RFC changes output selection, not the workspace lifecycle model, tool
arguments, stored historical messages, or locator API.

## Prompt Responsibilities

The reporting contract owns the final-message requirements and the rule that a
file entry point cannot replace a self-contained result. Workspace tool
guidance owns the concrete default reference form and lifecycle details.

During the contract phase, reporting guidance should:

- require self-contained results;
- require confirmed location metadata;
- direct the model to surface-specific guidance rather than a universal path
  form;
- retain the current concrete workspace-reference guidance until the consumer
  gate passes.

After the consumer gate passes, workspace guidance can adopt the location
matrix while retaining historical `workspace://` compatibility instructions.

## Consumer Acceptance Gate

The default prompt switch requires executable evidence for:

- a same-root project Markdown link resolved relative to the document;
- a cross-root local link that preserves the physical worktree identity;
- Holon brief links and images using confirmed absolute targets;
- public output that does not leak an execution-host path;
- canonical and worktree files with the same relative path remaining distinct;
- historical `workspace://` references preserving their root selector;
- missing files, removed roots, and foreign roots failing without fallback;
- spaces, parentheses, literal `#`, `%`, Unicode, and fragments remaining
  distinguishable.

The evidence must cover the relevant HTTP, Explorer, provider, and Web GUI
consumers. An issue or pull request being closed is not sufficient by itself.

## Non-Goals

- implementing the file resolver, Explorer, provider, or Web GUI consumer;
- migrating historical briefs or assistant messages;
- treating a filesystem path as a public URL or access capability;
- guaranteeing that every Markdown renderer makes an absolute path clickable;
- defining Windows paths or a line-number syntax in the initial rollout.
