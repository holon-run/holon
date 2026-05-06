# Tool Contract Implementation Notes

Related handles:

- `rfc-tool-contract-consistency`
- `rfc-tool-result-envelope`
- `rfc-tool-surface-layering`
- `rfc-command-tool-family`
- `rfc-exec-command-batch`
- `rfc-apply-patch-unified-diff-contract`
- `rfc-task-surface-narrowing`
- `rfc-interactive-command-continuation`

## Current implementation posture

The tool plane has converged around bounded model-visible receipts and richer
canonical results. Command startup, command-task continuation, task status,
task output, workspace binding, and file mutation are distinct surfaces rather
than one overloaded shell channel.

The remaining work is mostly consistency work. Individual tools can still drift
in naming, startup shape, result wording, or error-envelope shape unless they
are audited as one tool plane.

## Open gaps

1. Keep startup inputs separate from task/result metadata across all command
   tools.
2. Keep model-visible previews bounded while preserving artifact refs for full
   output.
3. Keep task lifecycle metadata out of raw output retrieval.
4. Keep ApplyPatch grammar/schema documentation aligned with the actual tool
   invocation surface.
5. Add contract tests for rejected invalid shapes, not only happy paths.
6. Ensure new tools identify which layer they belong to: control plane,
   workspace plane, task plane, waiting plane, or web/external plane.

Tracked by #913 for task result envelopes and artifact references.
