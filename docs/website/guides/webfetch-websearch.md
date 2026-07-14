---
title: WebFetch and WebSearch guide
summary: Agent tools for fetching web pages and searching the web — tool reference, extract modes, search providers, and usage patterns.
order: 45
---

# WebFetch and WebSearch Guide

Holon agents have two built-in web tools that enable them to retrieve and
search the public web. These tools are part of the `Web` capability family and
are available to every agent by default.

## WebFetch

`WebFetch` fetches a specific HTTP or HTTPS URL, extracts readable text, and
returns structured provenance. The agent uses it to read web pages,
documentation, or API responses.

### Parameters

| Parameter | Required | Description |
|-----------|----------|-------------|
| `url` | yes | HTTP or HTTPS URL to fetch |
| `max_chars` | no | Maximum characters to return (default: no hard limit) |
| `extract_mode` | no | How to extract content from the response |

### Extract modes

| Mode | Behavior |
|------|----------|
| `auto` (default) | Detect content type: render HTML to text, pass text through unchanged |
| `text` | Strip HTML tags and return plain text |
| `raw` | Return the raw response body without processing |

### What WebFetch returns

Each response includes provenance metadata:

- Final URL (after redirects)
- HTTP status code
- Content type
- Truncation flag and character count
- Content hash (SHA-256)

The fetched content is treated as **untrusted external content** by the
runtime. The agent receives the provenance wrapper and is instructed not to
escalate trust based on fetched content alone.

### Example usage

An agent calls WebFetch like any other tool:

```
WebFetch { url: "https://example.com/docs/api", max_chars: 5000 }
```

The runtime fetches the URL, applies Holon's web policy, extracts readable
text, and returns the result.

## WebSearch

`WebSearch` searches the web through Holon's web provider registry and returns
structured results with citations.

### Parameters

| Parameter | Required | Description |
|-----------|----------|-------------|
| `query` | yes | Search query string |
| `max_results` | no | Maximum number of results to return |
| `provider` | no | Search provider to use (default: configured provider) |

### Search providers

Holon uses a provider-based search model with multiple provider options:

- **DuckDuckGo (managed)** — Holon's built-in managed search provider. No
  API key required. Enabled during onboarding with "Managed DuckDuckGo" or
  "Auto" mode.
- **Tencent Cloud WSA** — Tencent Cloud SearchPro / Web Search API. Requires an
  API key set via credential profile. Configure with
  `holon config providers set tencent --kind tencent_cloud_wsa --credential-profile <profile>`.
- **Bocha AI Search** — Bocha AI Web Search API. Requires an API key set via
  credential profile. Configure with
  `holon config providers set bocha --kind bocha --credential-profile <profile>`.
- **Model-native search** — Some model providers (OpenAI, Anthropic) support
  native web search through their own APIs. In "Auto" mode, Holon prefers
  these when available.
- **Bing Web Search** — Microsoft Bing Web Search API. Requires an API key
  set via credential profile. Configure with
  `holon config set providers bing --kind bing --credential-profile <profile>`.

Search results from all providers are standardized into a consistent format
with title, URL, and snippet text. Citations are preserved so agents can
follow up with `WebFetch` for full page content.

Search configuration is part of onboarding and can be changed later with
`holon config set`.

### What WebSearch returns

Each result includes:

- Title and URL
- Snippet or summary text
- Source attribution

Results are structured so the agent can follow up with `WebFetch` to read full
pages when needed. The tool description explicitly tells the agent: "Use
WebFetch after search when full page content is needed."

### Example usage

```
WebSearch { query: "Rust async runtime design patterns", max_results: 5 }
```

## Web policy

Holon applies a configurable web policy to all fetch and search operations:

- **Allowed schemes**: `http` and `https` only
- **Domain filtering**: configurable allow/deny lists
- **Timeout**: configurable per-request timeout
- **Redirects**: followed up to a configurable limit

The policy is controlled through Holon's configuration and applies uniformly
to both WebFetch and WebSearch.

## When agents use these tools

Agents decide when to use web tools based on the task context. Common
patterns:

- **Research**: Agent uses WebSearch to find information, then WebFetch to
  read specific pages.
- **Documentation lookup**: Agent fetches API docs, RFCs, or package
  documentation from the web.
- **Verification**: Agent cross-references claims against public sources.

The runtime exposes both tools in the model-facing tool schema, and the agent
selects them through normal tool-calling when the task requires web access.

## XSearch

`XSearch` searches public X (Twitter) posts using xAI's hosted `x_search`
endpoint. It operates as an isolated provider request — independent of the
main conversation model — and returns durable text with citations.

### When to use XSearch

Use XSearch for X-specific content, accounts, or discussions. Use `WebSearch`
for the general web.

### Parameters

| Parameter | Required | Description |
|-----------|----------|-------------|
| `query` | yes | Search query string |
| `allowed_x_handles` | no | Restrict results to these X handles (max 10, without `@`) |
| `excluded_x_handles` | no | Exclude results from these X handles (max 10, without `@`) |
| `from_date` | no | Start date in `YYYY-MM-DD` format |
| `to_date` | no | End date in `YYYY-MM-DD` format |

### What XSearch returns

| Field | Description |
|-------|-------------|
| `text` | Search result text from the model response |
| `citations` | Structured citations with URL, title, and text position indices |
| `provider` | Always `xai` |
| `backend` | Always `x_search` |
| `model` | xAI model used for the search |
| `diagnostics` | Provider request ID, latency, and hosted item type counts |

### Prerequisites

XSearch requires:

1. **xAI provider configured** with `openai_responses` transport and
   valid credentials (OAuth device login through Codex, or API key).
2. **XSearch enabled** (enabled by default when xAI credentials are
   available).

To disable XSearch:

```bash
holon config set x_search.enabled false
```

### Configuration

XSearch configuration uses these keys:

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `x_search.enabled` | boolean | `true` | Enable XSearch when xAI credentials are available |
| `x_search.model` | model_ref | `grok-4.3` | xAI model route for isolated XSearch requests |
| `x_search.timeout_seconds` | integer | `60` | Request timeout in seconds |

The default model is `grok-4.3`. XSearch uses the xAI provider's OAuth
credentials and refreshes tokens only on 401 Unauthorized responses.

### Example usage

```
XSearch { query: "Holon runtime agent framework", from_date: "2026-01-01" }
```

## Configuration

Web tools are controlled through Holon's web configuration section:

```bash
# Check current web configuration
holon config get web

# Disable web tools entirely
holon config set web.enabled false
```

See [Configuration reference](/reference/configuration.md) for the full web
configuration schema.

## See also

- [Model tool schema inventory](/reference/model-tool-schema-inventory.md) — tool registration and stability
- [Integration guide](/guides/integration.md) — HTTP control plane and webhooks
- [Configuration reference](/reference/configuration.md) — web policy settings
