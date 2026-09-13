---
title: Documentation workflow
summary: How to edit and build the mdorigin-powered Holon website.
order: 20
---

# Documentation workflow

The Holon website is a mdorigin content root under `docs/website/`. Source
pages are Markdown files that can render as HTML or be fetched as Markdown.

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
python3 docs/website/.tools/check-links.py
python3 docs/website/.tools/test-check-contract-refs.py
python3 docs/website/.tools/check-contract-refs.py
```

Failures use `file:line:target` diagnostics so editors and CI logs can locate
the declaration directly.

## Preview locally

```bash
cd docs/website
mdorigin dev --root .
```

## Refresh indexes

Directory pages may contain managed index blocks:

```markdown
<!-- INDEX:START -->
<!-- INDEX:END -->
```

Regenerate them with:

```bash
mdorigin build index --root .
```

## Build deployable assets

```bash
mdorigin build search --root . --out dist/search
mdorigin build cloudflare --root . --search dist/search
```

The generated `dist/` directory is ignored and should not be committed.

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

## Refresh generated contract snapshots

OpenAPI, HTTP route, CLI, runtime status enum, and model tool schema snapshots
are checked separately by the main CI. Run `make snapshots-check` before
publishing a contract change. When a change is intentional, run
`make snapshots-refresh`, review the generated diff, and then rerun
`make snapshots-check`.

## Publishing note

`siteUrl` is configured as `https://holon.run` so publishing exposes canonical
sitemap and feed URLs for the production domain.
