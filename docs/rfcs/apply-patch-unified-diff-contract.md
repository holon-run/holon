---
title: RFC: ApplyPatch Unified Diff Contract
date: 2026-04-26
status: draft
---

# RFC: ApplyPatch Unified Diff Contract

## Summary

Holon should make standard unified diff the model-visible patch format for
`ApplyPatch`.

Provider transports may expose different outer tool shapes:

- OpenAI/Codex-compatible providers can expose `ApplyPatch` as a custom
  freeform grammar tool.
- Anthropic-compatible providers expose `ApplyPatch` as a JSON tool with a
  `patch` string parameter.

Those provider-level differences should not require the model to learn two
different patch languages. The text inside the patch should be the same
unified diff subset in both cases.

## Problem

Holon currently uses a Codex-style custom patch DSL:

```patch
*** Begin Patch
*** Update File: src/main.rs
@@
 old context
-old line
+new line
*** End Patch
```

That DSL is workable when the provider can enforce a freeform grammar during
tool decoding. It is less reliable when the provider only validates the outer
JSON tool shape and treats the patch body as an unconstrained string.

In Anthropic-compatible runs, the model has repeatedly produced patch bodies
that look like standard unified diff:

```diff
--- a/tests/foo.rs
+++ b/tests/foo.rs
@@ -10,3 +10,4 @@
 context
-old
+new
```

This is a natural model prior: `patch`, `diff`, and `hunk` usually imply
unified diff or git diff. Holon's private DSL therefore creates an avoidable
format mismatch. The result is failed `ApplyPatch` calls, fallback shell edits,
extra test-fix loops, larger token usage, and less predictable benchmark
behavior.

## Goals

- use one model-visible patch format across providers
- align the patch body with the model's strongest diff prior
- keep provider transports thin and provider-specific only at the outer tool
  call layer
- preserve Holon-owned path validation, workspace scoping, atomic writes, and
  structured receipts
- support a grammar-constrained OpenAI/Codex path without requiring Anthropic to
  support freeform tools
- make error messages specific to unified diff mistakes and apply failures

## Non-goals

- do not preserve backward compatibility with the current `*** Begin Patch`
  DSL for this proposal
- do not shell out to `patch(1)` or `git apply` as the authority
- do not support every git patch feature in the first version
- do not treat model-written line numbers as the only source of truth
- do not solve binary patching, file mode changes, submodule diffs, or copy
  detection

## Provider Contract

The provider-level wire shape is separate from the patch body contract.

### OpenAI/Codex-Compatible Providers

OpenAI Responses custom tools should expose `ApplyPatch` as a freeform grammar
tool. The grammar should describe Holon's supported unified diff subset.

The model should emit the unified diff body directly:

```diff
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@
 fn main() {
-    old_call();
+    new_call();
 }
```

### Anthropic-Compatible Providers

Anthropic Messages tools should keep a JSON tool schema:

```json
{
  "patch": "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    old_call();\n+    new_call();\n }\n"
}
```

Anthropic does not need, and currently should not claim, freeform grammar
support. The same unified diff body is simply carried inside the JSON `patch`
string.

## Unified Diff Subset V1

Holon should support a focused text patch subset rather than the full git patch
language.

### Modify File

```diff
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,5 +10,6 @@
 fn main() {
     println!("hello");
-    old_call();
+    new_call();
+    extra_call();
 }
```

Rules:

- `---` identifies the old file path.
- `+++` identifies the new file path.
- paths may use `a/` and `b/` prefixes, which Holon strips for workspace
  resolution.
- each file patch must have one or more hunks.
- context lines start with a single space.
- removed lines start with `-`.
- added lines start with `+`.

### Add File

```diff
--- /dev/null
+++ b/docs/new.md
@@ -0,0 +1,3 @@
+line 1
+line 2
+line 3
```

Rules:

- old path must be `/dev/null`.
- new path must resolve inside the active workspace.
- all hunk body lines must be additions or accepted no-newline markers.

### Delete File

```diff
--- a/docs/old.md
+++ /dev/null
@@ -1,3 +0,0 @@
-old line 1
-old line 2
-old line 3
```

Rules:

- new path must be `/dev/null`.
- old path must resolve inside the active workspace.
- all hunk body lines must be removals, context, or accepted no-newline
  markers.

### Rename

Holon should support the minimal git extended rename form.

Rename-only:

```diff
diff --git a/old.txt b/new.txt
similarity index 100%
rename from old.txt
rename to new.txt
```

Rename with edits:

```diff
diff --git a/old.txt b/new.txt
rename from old.txt
rename to new.txt
--- a/old.txt
+++ b/new.txt
@@ -1,3 +1,3 @@
 line 1
-old line
+new line
 line 3
```

Rules:

- `rename from` and `rename to` are the authoritative rename semantics.
- `similarity index` may be accepted and ignored.
- `index` may be accepted and ignored.
- if `---` and `+++` are present, their paths must agree with the rename paths.
- copy headers, mode changes, binary patches, and submodule patches are not part
  of V1.

## Apply Semantics

Holon should parse unified diff syntax, then apply changes using a
context-first algorithm.

The high-level policy is:

- be permissive while parsing common git diff metadata
- be strict about workspace safety and ambiguous edits
- apply only Holon-owned text and path semantics in V1
- ignore unsupported metadata instead of failing when it is safe to do so
- make ignored metadata observable in the canonical result

Line numbers in hunk headers are useful, but they should be hints rather than
the only authority:

```diff
@@ -42,7 +42,8 @@
```

The apply layer should:

- parse `old_start`, `old_count`, `new_start`, and `new_count`
- use `old_start` as the first search hint
- reconstruct the old hunk body from context and removed lines
- reconstruct the new hunk body from context and added lines
- first try to match near the hinted line
- fall back to full-file context matching when the hinted location misses
- fail when the old hunk body has no match
- fail when the old hunk body has multiple plausible matches
- apply all hunks for a file atomically
- write all files atomically for the whole tool call when practical
- treat hunk line counts as advisory when context matching succeeds
- reject duplicate file patches for the same normalized path

This keeps the model-facing format standard while preserving agent-friendly
robustness. Models can write familiar unified diff headers, but small line
number drift does not need to make a valid contextual edit fail.

## Metadata Policy

V1 should follow a "wide in, narrow out" rule.

Holon should accept common git diff metadata that models are likely to include,
but only execute semantics that are deliberately supported by `ApplyPatch`.

Accepted and ignored metadata:

- `index ...`
- `similarity index ...`
- `new file mode ...`
- `deleted file mode ...`
- `old mode ...`
- `new mode ...`

Ignored metadata must not change filesystem permissions or other file metadata.
For example, `new file mode 100755` should not run `chmod` or mark the file
executable in V1.

The reason is boundary control. `ApplyPatch` V1 is a text and path mutation
tool, not a general filesystem metadata tool. Mode changes expand the safety
surface, vary across platforms, and are often copied from git output without the
model intending a permission change.

Canonical results should record ignored metadata when present, for example as
`ignored_metadata`, so benchmarks and debugging can distinguish accepted
metadata from applied semantics.

## Grammar Sketch

The OpenAI/Codex custom tool grammar should describe the supported subset, not
the full git diff language.

Illustrative Lark shape:

```lark
start: file_patch+

file_patch: git_header? rename_header? old_file new_file hunk+
          | git_header rename_header

git_header: "diff --git " path_token " " path_token LF
rename_header: metadata_line* rename_from rename_to metadata_line*
rename_from: "rename from " filename LF
rename_to: "rename to " filename LF
metadata_line: ("similarity index " /[^\n]+/
              | "index " /[^\n]+/
              | "new file mode " /[^\n]+/
              | "deleted file mode " /[^\n]+/
              | "old mode " /[^\n]+/
              | "new mode " /[^\n]+/) LF

old_file: "--- " file_path LF
new_file: "+++ " file_path LF
file_path: "/dev/null" | path_token
path_token: ("a/" | "b/")? filename
filename: /[^\n]+/

hunk: hunk_header hunk_line+
hunk_header: "@@ -" range " +" range " @@" /[^\n]*/ LF
range: INT ("," INT)?

hunk_line: context_line | add_line | remove_line | no_newline
context_line: " " /[^\n]*/ LF
add_line: "+" /[^\n]*/ LF
remove_line: "-" /[^\n]*/ LF
no_newline: "\\ No newline at end of file" LF

%import common.INT
%import common.LF
```

The production grammar should be stricter where needed for provider decoding
stability, but the runtime parser remains the authority for workspace safety
and semantic validation.

## Error Contract

`ApplyPatch` errors should distinguish syntax, path, and apply failures.

Examples:

- `missing_file_header`: expected `--- old_path` followed by `+++ new_path`
- `invalid_hunk_header`: expected `@@ -old_start,old_count +new_start,new_count @@`
- `unsupported_git_patch_feature`: binary patches, mode changes, copy headers,
  or submodule patches are not supported; harmless metadata may be accepted and
  ignored instead
- `path_escape`: patch path resolves outside the workspace
- `rename_path_mismatch`: rename headers disagree with `---` / `+++` paths
- `context_not_found`: hunk context does not match the current file
- `ambiguous_context`: hunk context matches multiple locations; include more
  surrounding context
- `duplicate_file_patch`: the same normalized file path appears in more than
  one file patch; merge hunks into a single file patch

Hunk count mismatches should not fail the patch when context matching succeeds.
They should be recorded as advisory diagnostics in the canonical result.

Error receipts shown back to the model should include a compact recovery hint
that names the specific unified diff rule that failed.

## Prompt Guidance

The model-facing guidance should avoid mixing provider freeform terminology with
the patch body format.

Recommended wording:

```text
Use ApplyPatch for file mutations.

The patch body must be unified diff text. Do not use the old
`*** Begin Patch` / `*** Update File:` format.

For Anthropic-style JSON tools, call ApplyPatch with:
{"patch":"--- a/path\n+++ b/path\n@@ -1,1 +1,1 @@\n-old\n+new\n"}

For OpenAI custom/freeform tools, emit the same unified diff body directly.
```

The prompt should also say that line numbers are expected but do not need to be
over-explained. The tool error path should handle drift and ask for rereading
the target region only when context matching fails.

## Rationale

Codex can use a private patch DSL because its OpenAI custom tool path can
constrain generation with a grammar. The grammar reduces the cost of a
non-standard format.

Holon has to support provider surfaces where only the outer JSON shape is
validated. In that environment, a private patch DSL depends more heavily on
prompt compliance. Standard unified diff better matches model priors and gives
both Anthropic and OpenAI the same patch language.

The proposed design keeps the best part of Codex's apply behavior: context-first
matching. It only changes the model-visible syntax from a private DSL to a
standard diff subset.

## Verification Plan

Implementation should include:

- parser unit tests for modify, add, delete, rename-only, and rename-with-edit
- path-safety tests for `../`, absolute paths, and prefix normalization
- context apply tests for exact line match, line-number drift, missing context,
  and ambiguous context
- provider request tests showing OpenAI emits a custom grammar tool and
  Anthropic emits JSON `input_schema`
- prompt snapshot tests showing Anthropic guidance uses `{"patch": "..."}`
  while OpenAI guidance uses the same unified diff body directly
- benchmark comparison of `ApplyPatch` failure rate and shell-edit fallback rate
  before and after the change

## Decisions

- Hunk line numbers and counts are advisory. Context matching is authoritative
  when it succeeds uniquely.
- Common mode metadata is accepted and ignored. V1 does not execute chmod or
  other metadata mutations.
- Multiple hunks for the same file are allowed inside one file patch. Multiple
  file patches for the same normalized path are rejected.
- Rename-only operations require `diff --git` plus `rename from` and
  `rename to`.
- `similarity index` and `index` are accepted and ignored.
- Unsupported high-risk features such as binary patches, copy headers,
  submodule patches, and executable mode application remain out of scope.
