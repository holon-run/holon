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

## Publishing note

`siteUrl` is configured as `https://holon.run` so publishing exposes canonical
sitemap and feed URLs for the production domain.
