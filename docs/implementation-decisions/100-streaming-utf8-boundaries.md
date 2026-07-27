# Streaming UTF-8 boundaries

Transport and process read chunks are byte boundaries, not text boundaries.

Holon decodes command output, OpenAI streaming bodies, and provider HTTP trace
text with one incremental UTF-8 decoder per independent byte stream. The
decoder retains incomplete code points between chunks and applies lossy
replacement only to malformed input or an incomplete sequence at end of
stream. OpenAI keeps its existing SSE framing, while Anthropic keeps its
existing strict UTF-8 event parsing.

ApplyPatch receives an already decoded Rust `String`, so it cannot recover the
original bytes or prove why U+FFFD is present. It therefore permits the patch
but emits a model-visible diagnostic, and failed patches add a recovery hint to
reread the exact target region before retrying.
