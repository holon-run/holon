# Provider-neutral output citations

## Choice

Represent provider citations as a dedicated `ModelBlock::Citations` block and
persist them on the canonical `BriefRecord`. Provider adapters must remove
provider-private citation markers from visible text and convert supported
annotations to URL/title metadata.

## Reason

A separate block preserves citation evidence in assistant-round transcripts
without changing every text block constructor or replaying provider-specific
annotation syntax into later model requests. Persisting the same metadata on
the brief keeps HTTP hydration and user-facing timelines aligned with the
canonical delivery object.

## Preserved boundary

Provider-local source ids and text offsets are not durable identifiers. Citation
blocks are ignored when encoding prior assistant content for another provider,
and malformed or unsafe metadata degrades to readable marker-free text rather
than becoming executable links.
