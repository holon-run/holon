# Shared system prompt prefix (#4214)

## Contract

System prompt layout and cache scope are independent. `SharedPrefix` means
audited content whose bytes depend only on the runtime contract version and
the actual tool inventory/call surface. It does not mean `Stable`, grant
cache permission, or establish trust. New and historical sections default to
`ScopedSuffix`; source paths and user-authored guidance are never inferred to
be shared by inspecting their text.

One ordered section sequence owns rendering, structured provider blocks,
fingerprints and inventory. Its initial, contiguous shared sections contain
the fixed identity skeleton, core runtime/engineering contracts, applicable
tool guides and generic App contract. Workspace identity, agent relationships,
turn-specific exceptions, loaded guidance, memory and skills remain in the
scoped suffix. Workspace paths and guidance source attribution are preserved,
not redacted or rewritten. Shared block separators remain identical whether
or not a suffix is present.

Tool guides precede loaded guidance physically. This is a deliberate exception
to the former `guidance < skills < tools` ordering, not a permission change:
user-global, agent, memory, workspace and skills keep their relative order.
Explicit instruction precedence, operator/turn exceptions and origin/trust
boundaries remain authoritative. The response-language contract remains the
last system section, even though its own text is fixed.

## Provider and cache boundaries

Providers consume the same layout without independently sorting sections.
Concatenating structured system block text must exactly reproduce the rendered
system string. OpenAI Chat/Responses and Gemini retain their protocol-specific
envelopes; Anthropic retains its native and Claude Code cache strategies.
Neither protocol adaptation nor diagnostics may prepend instance-dependent
content before the shared prefix.

Anthropic's first system breakpoint ends the explicit shared prefix, not the
last `Stable` section. At most one additional breakpoint ends the last
cacheable system/context block. Coincident markers are deduplicated; absent
shared content is not inferred to be shared. `TurnScoped` blocks do not
receive these markers. This leaves two of the four available breakpoints for
frozen turn context and rolling history, as specified in
[the turn-scoped prefix contract](anthropic-turn-scoped-cache-prefix.md).
Disabling cache control omits all explicit wire markers without dropping text.
Replay history, turn freezing and time reminders do not change.

Complete fingerprints include layout and scoped content. Agent/session cache
keys retain their existing isolation. Shared-prefix digests, lengths and
boundary identifiers are observations only, never cache identities or
authorization. Counts are characters/bytes, not tokenizer-independent tokens.
Input budgets and clipping behavior are unchanged.

## Acceptance and limitations

Two independent agents/workspaces/tasks with identical contract/tool
conditions must have byte-identical shared prefixes containing core and tool
guides, while retaining distinct scoped suffixes and complete identities.
Different tool schemas, order, invocation surfaces, models or provider routes
are not promised to be cache-compatible. Unknown historical layouts stay
conservative. Empty suffix/context, both Anthropic strategies, cache on/off,
materialized/unmaterialized context and consecutive rounds must preserve the
same ordering and breakpoint budget.

Deterministic rendering and captured-wire tests establish structural reuse,
not backend cache hits or latency savings. Real-model comparisons must keep
model, tools and configuration fixed, use independent tasks, distinguish
model residency from prompt caching, and report measured input/cache usage
and latency with sample counts and unavailable metrics explicitly identified.
No shared agent cache key, higher input budget or duplicated instructions may
be used to manufacture an improvement.

## Real-model evidence (2026-11-17)

The ignored `tests/live_shared_prompt_prefix.rs` probe completed exactly six
requests on `deepseek@default/deepseek-flash` (native Anthropic Messages).
Both arms used the same 42 tools, minimum advertised reasoning effort (`low`)
and 128 output-token cap. Each request had an independent agent, task,
workspace, provider instance and complete cache identity. The control only
moved the workspace section ahead of the remaining shared sections; section
content was unchanged. This is a layout control, not a historical binary.

| Layout | Sample | Reported input tokens | Cache-read tokens | Output tokens | Elapsed ms |
| --- | --- | ---: | ---: | ---: | ---: |
| Dynamic-first control | First | 23,161 | 0 | 7 | 3,471.68 |
| Dynamic-first control | Warm 1 | 23,161 | 0 | 41 | 3,029.73 |
| Dynamic-first control | Warm 2 | 23,161 | 0 | 7 | 1,346.42 |
| Canonical shared prefix | First | 23,164 | 0 | 7 | 1,519.13 |
| Canonical shared prefix | Warm 1 | 11,644 | 11,520 | 7 | 1,681.05 |
| Canonical shared prefix | Warm 2 | 11,644 | 11,520 | 7 | 1,280.04 |

All six returned the exact requested short result with no tool calls.
Provider-reported cache-creation tokens were zero throughout. First-body
timing was unavailable; elapsed duration is not TTFT. Input and cache-read
columns retain the provider's separate usage fields.

The explicit shared prefix grew from 116 to 56,472 characters; the full
system's common initial text across each arm's three scopes grew from 251 to
56,609 characters. The two warm samples reported 23,040 total cache-read
tokens for the canonical layout versus zero for the control. Warm mean
duration was 1,480.54 ms versus 2,188.08 ms. These are observations, not a
general latency or cache-hit guarantee: arm order was fixed, model residency
was uncontrolled, shared tools could also cache, and each arm had only two
warm samples. The live fixture mirrors the private runtime breakpoint helper;
it reuses canonical rendering but does not replace production captured-wire
tests or establish end-to-end runtime performance.
