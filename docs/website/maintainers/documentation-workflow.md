---
title: Documentation workflow
summary: How to edit and build the mdorigin-powered Holon website.
order: 20
---

# Documentation workflow

The Holon website sources live under `docs/website/`. Builds prepare an isolated
mdorigin content root in `.tools/dist/content/`; source pages are Markdown files
that can render as HTML or be fetched as Markdown.

## Edit content

Add or update Markdown files in `docs/website/`. Use directory `README.md`
files for section landing pages:

```text
docs/website/
  README.md
  concepts/
    README.md
    runtime-model.md
  guides/
    README.md
```

Keep public-facing website copy concise. Detailed runtime contracts and design
records still belong under the repository `docs/` tree.

Use [Documentation layers](documentation-layers.md) to decide whether a change
belongs in Concepts, a task guide, Reference, or a maintainer contract.

## Check contract references

The docs CI checks repository-local Markdown links and heading anchors in:

- `README.md`, `docs/architecture-overview.md`, and `docs/runtime-spec.md`
- `docs/website/spec/` and `docs/website/reference/`
- real Markdown links in `docs/rfcs/`

Focused specs may also declare implementation paths in their `Last verified`
quote or an `Implementation references` section. Write repository paths there
as code spans, for example:

```markdown
> **Last verified:** against `src/runtime/scheduler.rs` and
> `src/runtime/waiting.rs`.
```

Elsewhere, use a real Markdown link when a source path should be enforced:

```markdown
[`src/http/mod.rs`](../../../src/http/mod.rs)
```

Fenced examples, placeholders, and ordinary code spans are not interpreted as
current implementation contracts. A historical or proposed reference on an
otherwise checked line can be excluded only by placing a non-empty reason
immediately after that specific reference:

```markdown
`src/runtime/proposed.rs` <!-- contract-ref-ignore: proposed file from accepted RFC -->
```

Run the checks from the repository root:

```bash
npm --prefix docs/website/.tools run prepare:content
npm --prefix docs/website/.tools run check:content
python3 docs/website/.tools/test-check-contract-refs.py
python3 docs/website/.tools/check-contract-refs.py
```

Failures use `file:line:target` diagnostics so editors and CI logs can locate
the declaration directly. Links and navigation are checked together against the
prepared content root, which includes generated routes.

## Preview locally

Use Node.js 22.23.4 or newer in the 22.x release line and Python 3.11 or newer.
From the repository root:

```bash
npm ci --prefix docs/website/.tools
npm --prefix docs/website/.tools run dev
```

`dev` prepares a startup snapshot and serves that generated content, not the
source tree. After any website content (including ordinary Markdown),
configuration, style, template, or display metadata change, restart the preview.
For content-only edits, running
`npm --prefix docs/website/.tools run prepare:content` separately refreshes the
generated files; configuration and stylesheet changes require a restart because
mdorigin loads them at startup. Source changes are not automatically copied;
there is no source-tree watcher.

## Refresh indexes

Documentation landing pages use one managed catalog as the source of article
links. Keep introductory prose and cross-section links outside it, but do not
copy the catalog into the body or translate it as a second list. Translate each
article's `title` and `summary`, then regenerate the index. Run
`npm --prefix .tools run check:directories` from `docs/website` after preparing
content and building indexes to check the generated directory pages for duplicates.

Directory pages may contain managed index blocks:

```markdown
<!-- INDEX:START -->
<!-- INDEX:END -->
```

Regenerate them in the isolated content root from `docs/website` with:

```bash
npm --prefix .tools run prepare:content
npm --prefix .tools run build:index
npm --prefix .tools run check:directories
```

## Build deployable assets

From `docs/website`:

```bash
npm --prefix .tools run build
```

The build prepares content once, then builds directory indexes, checks generated
links and directories, and builds search and Cloudflare assets from that same
content root. Search and bundle outputs remain in `.tools/dist/search/` and
`.tools/dist/cloudflare/`. The generated `.tools/dist/` tree is ignored; do not
commit it or copy generated catalogs back into the source pages.

## Template catalog

The catalog reads repository-local `agent_templates/*/template.toml` and
`skills.toml` files. `.tools/build-template-catalog.py` copies website content
(excluding `.tools`) and generates the home-page selections, full catalog pages,
and JSON assets in `.tools/dist/content/`. It works from any current directory.
There is no network fetch or catalog API: each website build uses template data
from the same Git commit SHA as its documentation.

Keep categories, featured ranks, and optional Chinese display names and summaries
in `template-display.json`; role contracts remain in the template files. Missing
localized display text falls back to the template's English metadata. Templates
without a display entry still appear in the full directory with a fallback
category. Removing a featured template fails the build until its stale selection
is removed or replaced in `template-display.json`.

CI validates templates and generator tests, retains contract-reference and
generated-copy checks, and checks all copied source and generated links and
navigation against the actual content root. Pull requests build and upload the
bundle but do not deploy.

## Generated pages and locales

Some pages are produced by a generator rather than written by hand. Today that
is `reference/models.md`, generated with:

```bash
cargo run --bin holon-docgen -- models > docs/website/reference/models.md
```

Do not hand-translate a generated page: the next regeneration would discard the
translation. Register the page in `.tools/generated-pages.json` instead, then
sync its localized copies:

```bash
npm --prefix .tools run sync:generated
```

The script copies the generated English page to each configured locale path,
replaces the listed front matter fields with localized values, and adds a notice
that the page body is generated. Commit the copy together with the regenerated
source page. Docs CI runs the same script with `--check` and fails when a
generated page and its copies disagree, so a regeneration needs one extra
command — never a new translation.

## Language detection

The site ships two locales: `en` (default) and `zh-CN`. `mdorigin.config.json`
enables `localeDetection`, so a request to `/` redirects once to the visitor's
preferred locale root. Deep links are never redirected, an explicit choice made
with the header language switcher wins, and crawler traffic stays on the default
root.

## Refresh generated contract snapshots

OpenAPI, HTTP route, CLI, runtime status enum, and model tool schema snapshots
are checked separately by the main CI. Run `make snapshots-check` before
publishing a contract change. When a change is intentional, run
`make snapshots-refresh`, review the generated diff, and then rerun
`make snapshots-check`.

## Publishing note

`siteUrl` is configured as `https://holon.run` so publishing exposes canonical
sitemap and feed URLs for the production domain.
