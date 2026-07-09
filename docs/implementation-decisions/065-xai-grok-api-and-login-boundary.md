# xAI Grok API and X Login Boundary

## Choice

Holon's first Grok integration uses the public xAI API as an OpenAI Responses
compatible HTTP provider authenticated by `XAI_API_KEY`.

Grok's built-in `web_search` and `x_search` are represented as xAI server-side
tools on that Responses request. They are not exposed as a raw Holon web-search
provider and they do not use X API search credentials.

X/xAI account login reuse is left out of the HTTP provider path. If Holon later
supports X-account-backed Grok sessions, it should integrate through an
official Grok Build surface such as the Grok CLI / ACP process boundary, not by
reusing browser cookies, private session files, or consumer subscription state
as an xAI API key.

## Reason

xAI's public inference API documents API-key authentication for direct
`/v1/responses` and `/v1/chat/completions` calls. Its built-in search tools are
server-side xAI tools on model requests.

Grok Build documents a separate login/session path for the official Grok CLI.
That path can reuse an X/xAI account for the CLI product, but it is not the same
credential contract as the public xAI API.

## Preserved boundary

The `xai` provider remains a normal remote HTTP provider with explicit API-key
credentials. Future X-account reuse must be a separate process/agent transport
with its own lifecycle and trust boundary, rather than hidden auth behavior in
the OpenAI-compatible transport.
