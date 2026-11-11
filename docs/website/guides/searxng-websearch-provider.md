---
title: Set Up Self-Hosted SearXNG as Holon WebSearch Provider
summary: "Deploy self-hosted SearXNG with Docker as a credential-free search backend for Holon WebSearch: network tuning, Holon provider configuration, and end-to-end troubleshooting."
order: 211
---

# Set Up Self-Hosted SearXNG as Holon WebSearch Provider

Connect a self-hosted SearXNG meta-search instance to Holon as the local data plane backend for the `WebSearch` tool. By hosting SearXNG locally, your agents can perform technical research and web searches without requiring commercial cloud search API keys, while preventing search terms from leaving your infrastructure. This setup is ideal for privacy-conscious environments or deployments that need predictable, zero-marginal-cost web discovery.

This guide is based on live, account-level self-hosted testing and end-to-end verification across isolated Holon instances. It covers single-container deployment with network tuning, Holon provider configuration, positive and negative test evidence, and troubleshooting. For general web tool workflows and security boundaries, see [Fetch and search web content](./use-web-tools.md) and the [Web tools reference](../reference/web-tools.md).

## Prerequisites

- A host machine with Docker installed and running.
- Holon 0.59.0 or higher (with built-in `kind = "searxng"` web provider support).
- Official SearXNG Docker image (tested with `searxng/searxng:2026.11.9-57ff753ad`).
- Outbound network connectivity from the host (for SearXNG to query upstream public search engines).

## Step 1: Deploy SearXNG (Docker)

### 1. Prepare `settings.yml`

Create a local directory and write the minimal working configuration. Explicitly enable JSON API format output and narrow the upstream engine list based on network reachability:

```yaml
# settings.yml
# Bind to loopback only; disable rate limiter for single-user local setups
use_default_settings:
  engines:
    keep_only:
      - bing
      - bing news
      - baidu

engines:
  - name: bing
    disabled: false
  - name: bing news
    disabled: false
  - name: baidu
    disabled: false

server:
  secret_key: "<generate with: openssl rand -hex 24>"
  limiter: false
  image_proxy: false

search:
  formats:
    - html
    - json        # Required: Holon's searxng provider requires format=json; omitting this causes HTTP 403
  timeout: 8.0
  # Shorten penalty periods so temporary network blips do not suspend healthy engines
  ban_time_on_fail: 5
  max_ban_time_on_fail: 30
```

Key configuration considerations:

- **`search.formats` must include `json`**: SearXNG disables JSON API output by default. Missing this setting causes all API calls to fail with an HTTP 403 HTML error page, which is the most frequent onboarding mistake.
- **Prune active engines**: Use `use_default_settings.engines.keep_only` to select reliable engines and override their state with `disabled: false`. Public engines have vastly different reachability profiles across datacenter and cloud networks; keeping hundreds of default engines enabled causes slow queries and excessive unresponsive engine warnings.
- **Avoid unconfigured keyed engines**: Engines marked `inactive: true` (such as Marginalia) require API keys to initialize. Because this guide focuses on a zero-credential setup, leave them unconfigured.

### 2. Start the container

Launch the container using a pinned image tag, host networking, and strict loopback binding:

```bash
docker run -d --name searxng \
  --network host \
  -e GRANIAN_HOST=127.0.0.1 \
  -e GRANIAN_PORT=8888 \
  -v "$PWD/settings.yml:/etc/searxng/settings.yml:ro" \
  searxng/searxng:2026.11.9-57ff753ad
```

Network and security details:

- **Why host networking is recommended**: In many datacenter network environments, Docker's default bridge NAT network can introduce packet drops and timeouts when connecting to certain upstream engines (such as Bing), even when the host itself reaches them without issue (1.1s–1.9s latency). Using `--network host` bypasses the bridge NAT and uses the host's network stack directly, eliminating "healthy instance but zero search results" issues.
- **Strict loopback binding**: Under host networking, applications may bind to `*:8888` by default. Because the official image uses Granian as its ASGI entrypoint, pass `-e GRANIAN_HOST=127.0.0.1` to restrict binding to the loopback interface (the legacy `SEARXNG_BIND_ADDRESS` variable is ignored by Granian). Verify binding with `ss -ltn | grep 8888`. If bridge networking is mandatory, use `-p 127.0.0.1:8888:8088` to constrain exposure, but monitor for upstream packet loss.

Verify the JSON API with a quick smoke test:

```bash
curl -s "http://127.0.0.1:8888/search?q=test&format=json" | grep -o '"results":\['
```

A matching output indicates that SearXNG is responding with valid JSON.

## Step 2: Configure Holon

You can configure Holon via the CLI or edit `config.json` directly.

### Option A: Use the Holon CLI

Run the following commands in your terminal:

```bash
# Register the SearXNG provider endpoint
holon config set web.providers.searxng.kind searxng
holon config set web.providers.searxng.base_url "http://127.0.0.1:8888"

# Switch the default WebSearch provider to searxng
holon config set web.search.enabled true
holon config set web.search.provider searxng
```

### Option B: Edit `config.json` directly

Add the provider and routing definitions under the `web` section of your Holon configuration:

```json
{
  "web": {
    "search": {
      "enabled": true,
      "provider": "searxng",
      "mode": "single"
    },
    "providers": {
      "searxng": {
        "kind": "searxng",
        "base_url": "http://127.0.0.1:8888"
      }
    }
  }
}
```

Provider features:

- **Zero credential management**: The `searxng` provider does not need a `credential_profile` or API key.
- **Path prefix support**: The `base_url` supports path prefixes (such as `https://internal.example.com/searxng/`); Holon automatically appends `/search` while preserving the prefix.
- **Consistent result normalization**: When an agent invokes `WebSearch`, Holon standardizes the result count using `max_results` (defaulting to 5). Titles, URLs, snippets, and citations follow the exact same schema as cloud providers.

## Step 3: End-to-end verification

### Positive test cases

Instruct your agent in conversation:

```text
Use WebSearch to search for "Holon runtime agent", and summarize the top 3 results with their source URLs.
```

The agent invokes `WebSearch { query: "Holon runtime agent", max_results: 5 }`. Verified test observations:

- **JSON API smoke test**: Tested across GitHub repositories, technical documentation, and news queries; each returned 10 structured items with ~0.6s response latency.
- **Isolated instance routing**: Verified on an isolated `HOLON_HOME` instance configured with `kind = "searxng"`. WebSearch correctly routed queries to the local instance and returned 5 structured results including official repositories and documentation.
- **Multi-query stability**: Executed 8 consecutive queries across distinct technical domains; 8 out of 8 succeeded, yielding 10–11 aggregated items per query (isolated upstream bot-detection challenges like Baidu CAPTCHAs degraded gracefully without failing the overall query).
- **Search-and-fetch closed loop**: The agent followed up with `WebFetch` on the returned URLs, successfully extracting page text, version numbers, and documentation tables.
- **Quality comparison**: Compared against commercial cloud providers (such as Brave) on the same queries, self-hosted SearXNG delivered broader domain diversity and sufficient precision for engineering workflows.

### Negative and error semantics verification

Verified error behaviors on intentional failure modes:

| Scenario | SearXNG server behavior | Holon agent observation |
|---|---|---|
| `search.formats` lacks `json` | HTTP 403 Forbidden (HTML error page) | Reports `network_failed` with explicit `HTTP 403 Forbidden` and `retryable: true` |
| `base_url` unreachable or port closed | Connection refused | Reports `network_failed` with full request URL and retry suggestions |
| All upstream engines timed out or challenged | HTTP 200 with valid empty results `{"results": []}` and `unresponsive_engines` list | Reports `parse_failed` ("no parseable search results") |

> **Note**: When all upstream engines time out or hit rate limits simultaneously, SearXNG returns HTTP 200 with an empty list. Holon categorizes this as `parse_failed` to signal that no usable results were parsed. A suggestion to clarify this diagnostic message is tracked in [holon-run/holon#4024](https://github.com/holon-run/holon/issues/4024). This reflects transient upstream availability rather than a configuration defect; configuring multiple diverse engines or retrying resolves it.

## Step 4: Troubleshooting

- **WebSearch reports `network_failed` with `HTTP 403 Forbidden`**:
  `settings.yml` inside the SearXNG container lacks JSON format support. Ensure `search.formats` includes `- json`, then restart the container.
- **WebSearch reports `network_failed` with connection refused**:
  Holon cannot reach the configured `base_url`. Run `curl -s "http://127.0.0.1:8888/search?q=test&format=json"` on the host to verify container health, and check host firewall rules.
- **WebSearch reports `parse_failed`, but manual curl sometimes succeeds**:
  The container is healthy, but all enabled engines timed out or were challenged during that specific query window. Inspect the `unresponsive_engines` field in the SearXNG response. To fix, switch the container to host networking to avoid NAT drops, increase `search.timeout` in `settings.yml` (e.g., 8–10 seconds), and lower `ban_time_on_fail`.
- **Intermittent empty results**:
  Single-engine suspension caused by timeout penalties. Enable at least two independent upstream engines in `keep_only` (such as Bing alongside a secondary engine) to provide redundancy.

## Step 5: Known limitations

- **Datacenter IP reachability varies by engine**: Public cloud and datacenter IP blocks are frequently blocked at the TLS handshake level or heavily rate-limited by privacy search engines (such as DuckDuckGo, Brave, or Startpage), while Baidu may trigger CAPTCHAs. In zero-credential setups, Bing-family engines offer the most reliable uptime. If additional engines are required, consider services with official API keys (such as Marginalia) and configure credentials in `settings.yml`.
- **Windowed failure on single-engine setups**: If only one engine is active and it experiences a temporary timeout, SearXNG suspends it, causing all subsequent queries to return empty results until the penalty expires. Always configure at least two independent engines in production.
- **Language coverage depends on active engines**: General engines like Bing cover both English and Chinese technical sources well. For niche locales or regional queries, explicitly enable regional engines.

## Step 6: Cleanup

To shut down and remove the SearXNG deployment:

```bash
# Stop and remove the container
docker rm -f searxng

# Remove any unused anonymous volumes
docker volume ls -q | grep searxng | xargs -r docker volume rm
```

To switch Holon back to another search provider:

```bash
holon config set web.search.provider duckduckgo
```

## See also

- [Fetch and search web content guide](./use-web-tools.md) — Trade-offs between search and fetch, and handling untrusted content
- [Web tools reference](../reference/web-tools.md) — Full parameter and provider inventory for WebFetch, WebSearch, and XSearch
- [Connect external events guide](./connect-external-events.md) — Wake agents using webhooks and external triggers
