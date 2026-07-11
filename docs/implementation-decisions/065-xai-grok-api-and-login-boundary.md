# xAI Grok API and X Login Boundary

## Choice

Holon's Grok integration uses the public xAI API as an OpenAI Responses
compatible HTTP provider authenticated by either `XAI_API_KEY` or a
Holon-managed official xAI OAuth profile.

Grok's built-in `web_search` and `x_search` are represented as xAI server-side
tools on that Responses request. They are not exposed as a raw Holon web-search
provider and they do not use X API search credentials.

xAI Responses requests use server-side response storage because
`previous_response_id` continuation requires the referenced response to remain
available. Continuation requests omit `instructions` to satisfy xAI's wire
contract; full requests retain them.

The OAuth profile is explicit provider credential material with refresh
lifecycle managed by Holon. Browser cookies, private consumer session files,
and implicit subscription-state reuse remain outside the HTTP provider path.

## Reason

xAI's public inference API supports direct authenticated `/v1/responses` and
`/v1/chat/completions` calls. Its built-in search tools are server-side xAI
tools on model requests.

Official OAuth and Grok Build login/session paths remain explicit credential or
process boundaries; neither authorizes scraping or silently importing browser
session state.

## Preserved boundary

The `xai` provider remains a normal remote HTTP provider with explicit API-key
or OAuth credentials. Any future consumer-session reuse must use an official
process/agent transport with its own lifecycle and trust boundary, rather than
hidden auth behavior in the OpenAI-compatible transport.
