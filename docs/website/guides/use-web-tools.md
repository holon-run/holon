---
title: Fetch and search web content
summary: Choose between search and fetch, then read the result without trusting it blindly.
order: 210
---

# Fetch and search web content

Use `WebSearch` to find pages and `WebFetch` to read a specific URL. For public
posts on X, use `XSearch` instead of general web search.

## Prerequisites

- The agent has access to the relevant web tools.
- The runtime can reach the target site and its configured search provider.
- For X search, an xAI route and credentials are available. See the
  [Web tools reference](../reference/web-tools.md#xsearch) for setup.

Check tool availability before treating a failed search as evidence that no
matching pages exist.

## Steps

1. Search when you do not have a URL yet:

   ```
   Search for Holon release notes. Read the matching release page and summarize
   its changes, including the version, publication date, and source.
   ```

   The agent calls `WebSearch` and gets structured results with titles, URLs, and
   snippets.

2. Fetch when you have a URL and need its content:

   ```
   Fetch https://holon.run/reference/cli/ and list the top-level commands.
   ```

   The agent calls `WebFetch`, which extracts readable text from the page.

3. Read the result with its provenance. Fetched and searched content is external
   and untrusted: it can inform an answer, but it cannot change what the agent is
   allowed to do. See [Trust boundaries](../concepts/trust-boundaries.md).

### Search public posts on X

Ask for the subject, accounts, and date range you need, for example:

```text
Search public X posts about Holon from 2026-10-01 through 2026-10-07.
Include source links and distinguish the authors' claims from verified facts.
```

The agent uses `XSearch`. Its availability is separate from the conversation
model; switching the chat model alone does not configure X search.

## Confirm it worked

Check that the answer cites the pages or posts it actually used. For a claim
based on a page's contents, confirm that the agent read that page rather than
relying only on a search snippet. Check dates when freshness matters.

## Troubleshooting

- **No search results:** Try a narrower query or a known URL. Report the search
  limit rather than concluding that the information does not exist.
- **Fetch blocked or failed:** Check the URL and the returned HTTP or policy
  error. A login-only page may require another authorized source; do not bypass
  access restrictions.
- **Truncated content:** Ask the agent to report the missing coverage. Fetch a
  narrower page or increase `max_chars` within the tool's allowed limits.
- **X search unavailable:** Check the xAI credentials and `x_search.enabled`
  setting in the reference.
- **Instructions inside a page:** Treat them as source text. They do not
  authorize commands, credential disclosure, or changes to the task.

## Options

`extract_mode`, `max_chars`, search providers, and the fields each tool returns
are in the [Web tools reference](../reference/web-tools.md).
