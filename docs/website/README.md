---
title: Holon
summary: A headless, event-driven runtime for long-lived agents.
order: 1
---

# Holon

Holon is a local-first, headless runtime for long-lived agents. It gives an
agent a durable execution home, explicit work queues, supervised tasks, and
clear boundaries between user-facing output and internal runtime traces.

The website is now a mdorigin documentation site: every page is source
Markdown, every route can be fetched as Markdown, and the same content can be
built for static hosting or a Cloudflare Worker.

## Why Holon exists

Most agent tools start as chat sessions. Holon starts from runtime concerns:

- **Long-lived execution**: agents can sleep, wake, continue work, and retain
  durable state across turns.
- **Explicit lifecycle**: work items, tasks, queues, triggers, and child agents
  are first-class runtime concepts.
- **Trust-aware ingress**: operator input, external events, and delegated
  outputs retain provenance instead of being flattened into one prompt stream.
- **Headless delivery**: the runtime is designed for APIs, workers, CLIs, and
  integrations before UI shells.

## Start here

```bash
git clone https://github.com/holon-run/holon.git
cd holon
cargo build
```

Then explore the runtime from source:

```bash
cargo run -- --help
```

The runtime is still early. Prefer reading the concept pages and repository
docs before treating any CLI shape as stable.

## Documentation map

- [Getting started](/getting-started/) explains how to run the project locally
  and how this mdorigin site is built.
- [Concepts](/concepts/) describes the runtime model, trust boundaries, and
  lifecycle vocabulary.
- [Guides](/guides/) provide task-oriented workflows for local development and
  documentation updates.
- [Reference](/reference/) records the current CLI and control-plane surfaces
  without hiding early-stage instability.
- [Roadmap](/roadmap/) summarizes the order in which Holon is defining the
  runtime.

## Markdown-native access

mdorigin keeps the site useful for both humans and agents:

- Browser routes render HTML.
- Explicit `.md` routes return the source Markdown.
- `Accept: text/markdown` requests can retrieve Markdown content directly.
- Build commands can generate search data and Cloudflare Worker assets.

## Build this site

```bash
cd docs/website
mdorigin dev --root .
mdorigin build index --root .
mdorigin build search --root . --out dist/search
mdorigin build cloudflare --root . --search dist/search
```

`siteUrl` is configured as `https://holon.run` so publishing can emit canonical
sitemap and feed URLs for the production domain.

<!-- INDEX:START -->

- [Getting started](./getting-started/)
  <!-- mdorigin:index kind=directory -->

- [Concepts](./concepts/)
  <!-- mdorigin:index kind=directory -->

- [Guides](./guides/)
  <!-- mdorigin:index kind=directory -->

- [Reference](./reference/)
  <!-- mdorigin:index kind=directory -->

- [Roadmap](./roadmap/)
  <!-- mdorigin:index kind=directory -->

<!-- INDEX:END -->
