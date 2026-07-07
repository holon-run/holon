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

Holon uses a provider-based search model:

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
