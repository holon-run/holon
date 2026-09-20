---
title: Fetch and search web content
summary: Choose between search and fetch, then read the result without trusting it blindly.
order: 19
---

# Fetch and search web content

Agents have two web tools. `WebSearch` finds candidate pages; `WebFetch` reads
one specific URL. Most research tasks use both, in that order.

## Steps

1. Search when you do not have a URL yet:

   ```
   Search for Holon release notes and summarize the latest changes.
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
   allowed to do. See [Trust boundaries](/concepts/trust-boundaries.md).

## Confirm it worked

The answer cites the pages it used. If a page was too large, `WebFetch` reports
that the content was truncated, and the agent can fetch a narrower URL or ask for
a character limit.

## Options

`extract_mode`, `max_chars`, search providers, and the fields each tool returns
are in the [Web tools reference](/reference/web-tools.md).
