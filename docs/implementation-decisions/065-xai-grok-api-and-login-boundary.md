# xAI Grok API and X Login Boundary

## Choice

Holon's Grok integration uses the public xAI API as an OpenAI Responses
compatible HTTP provider authenticated by either `XAI_API_KEY` or a
Holon-managed official xAI OAuth profile.

Grok's hosted `x_search` is exposed through Holon's PascalCase `XSearch`
client-side tool. `XSearch` sends a separate xAI Responses request containing
only the hosted `x_search` tool, then returns durable final text, citations,
and bounded diagnostics. It does not use X API search credentials.

xAI Responses requests use server-side response storage because
`previous_response_id` continuation requires the referenced response to remain
available. Continuation requests omit `instructions` to satisfy xAI's wire
contract; full requests retain them.

The OAuth profile is explicit provider credential material with refresh
lifecycle managed by Holon. Browser cookies, private consumer session files,
and implicit subscription-state reuse remain outside the HTTP provider path.
Holon-managed general web search remains the PascalCase `WebSearch` function
tool. Lowercase names such as `x_search`, `x_semantic_search`, and
`x_keyword_search` are not client-callable Holon tools. The lowercase
`x_search` type is valid only inside the isolated `XSearch` provider request;
hosted/internal call items are recorded as diagnostics rather than entering
the main agent tool execution or continuation chain.

`XSearch` is available only while the `xai` provider has usable credentials.
It is enabled automatically in that case, may be disabled with
`x_search.enabled = false`, and may select an independent xAI model with
`x_search.model`. Main xAI agent Responses requests do not advertise hosted
search.

X/xAI account login reuse is left out of the HTTP provider path. If Holon later
supports X-account-backed Grok sessions, it should integrate through an
official Grok Build surface such as the Grok CLI / ACP process boundary, not by
reusing browser cookies, private session files, or consumer subscription state
as an xAI API key.

## Reason

xAI's public inference API supports direct authenticated `/v1/responses` and
`/v1/chat/completions` calls. Isolating its hosted search in a client tool
keeps provider-internal calls and response continuation state out of the main
agent conversation while preserving searchable output for replay.

Official OAuth and Grok Build login/session paths remain explicit credential or
process boundaries; neither authorizes scraping or silently importing browser
session state.

## Preserved boundary

The `xai` provider remains a normal remote HTTP provider with explicit API-key
or OAuth credentials. Any future consumer-session reuse must use an official
process/agent transport with its own lifecycle and trust boundary, rather than
hidden auth behavior in the OpenAI-compatible transport.
