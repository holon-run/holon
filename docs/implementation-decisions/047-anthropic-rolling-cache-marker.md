# 047 Anthropic Rolling Cache Marker

Anthropic rolling conversation cache markers are provider lowering, not runtime
prompt-frame state.

Holon keeps the runtime conversation as the replayable semantic history. The
Anthropic transport may add one request-local `cache_control` marker to the
latest cacheable conversation content block when lowering that history to the
Messages API wire shape. This keeps the growing conversation tail cacheable
without mutating shared conversation state or teaching non-Anthropic transports
about Anthropic cache-control placement.
