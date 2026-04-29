# 049 Anthropic Cache Break Classification

Anthropic prompt-cache break classification is benchmark diagnostics, not
runtime behavior.

Holon records raw provider request/cache facts in the runtime transcript. The
benchmark runner derives per-round classifications from those facts when it
builds `token-optimization.json`, then copies aggregate counts into
`metrics.json` and `summary.json` through the existing token optimization
summary path.

This keeps cache-miss explanations close to benchmark artifacts while preserving
the runtime boundary: classifications may explain request-shape changes, TTL
possibility, compaction boundaries, or likely server-side cache misses, but they
must not feed back into prompt assembly, scheduling, or provider request
lowering.

The classifier compares the stable request surface and intentionally ignores
normal rolling conversation-tail growth at the coarse-shape layer. Tail marker
content and position move as the conversation advances, so treating that
movement as client shape churn would hide the server-side cache misses this
diagnostic is meant to identify.

For Anthropic, the benchmark also records provider-payload cache breakpoint
diagnostics immediately before send. Each breakpoint includes its provider path,
block kind, estimated prefix tokens, a secret-safe content hash, and a canonical
prefix fingerprint for the serialized provider payload prefix up to that
breakpoint. The benchmark derives per-round reuse flags from these fingerprints
inside a stable-shape segment; the runtime does not use them for prompt
assembly.

Classification uses the last positive cache-read baseline in the stable-shape
segment instead of only the immediate previous round. This keeps a
positive-read -> zero-read -> zero-read sequence inspectable as an initial drop
followed by a continued miss. If Anthropic reports response-level
`context_management.applied_edits`, the benchmark records those edits as
diagnostic evidence and classifies material cache-read drops on that round as
`context_management_applied`.
