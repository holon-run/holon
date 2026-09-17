# Holon lite paper

This directory owns the editable content and PDF build tools. The paper is still
under review; the Chinese version is being developed first. No website download
has been published by this change.

## Files and ownership

| Path | Purpose | Commit? |
| --- | --- | --- |
| `zh-CN.md` | Chinese reader-facing copy; the PDF's content source | Yes |
| `en.md` | English counterpart, created when translation starts | Later |
| `editorial.md` | Open decisions, supporting evidence and editing notes | Yes |
| `assets/` | Shared diagram sources and necessary images | Yes |
| `tools/build.py` | Locale selection, source reading and build metadata | Yes |
| `tools/layout.py` | Shared typography and Markdown block layout | Yes |
| `tools/requirements.txt` | Pinned Python dependencies | Yes |
| `tools/fonts/` | Font setup; future licensed distribution fonts | Docs now |
| `../../build/lite-paper/<lang>/` | Review PDF, previews and build metadata | No |
| `../website/assets/lite-paper/` | Reviewed publication PDFs | On approval |

The previous v0.1/v0.3 PDFs, fixed-page scripts and diagnostics have been retained
locally under `build/lite-paper/history/`. The latest six-page script is also
preserved in `tools/archive/layout-v0.3.py` for its vector-layout implementation.
It is not a build entry point or active content source. Historical paths inside
archived scripts/notes are not maintained.

## Edit the content

Edit `zh-CN.md` and rebuild. Product copy is not duplicated in Python. Keep
internal decisions and unverified numerical claims in `editorial.md`.

The source begins with a small `key: value` metadata block delimited by `---`.
Supported fields are `lang`, `version`, `updated`, `status` and `title`; these are
plain strings, not a general YAML document. Filenames remain stable; revisions
belong in metadata and Git history.

The intermediate renderer supports the subset currently used by the paper:

- headings at levels 1–3, paragraphs, bold, emphasis and inline code;
- links, quotes, flat bullet lists and simple Markdown tables;
- fenced code and text diagrams.

Level-2 sections start on a fresh page after the opening section. Longer sections
can span pages; the renderer does not silently shorten the text to force six
pages. It retains the teal/off-white typography of the prior mock-up, but the
earlier six-page bespoke diagrams remain a visual reference for the next layout
pass. Edit paragraph length or the shared layout to tune pagination.

Local links into `docs/website/` become public `https://holon.run/` links in PDFs.
Other local source links fail the build rather than expose local filesystem
paths. Unsupported complex Markdown should be added deliberately to the renderer.

## Build and review

From the repository root, with Python 3.10 or newer:

```bash
python3 -m venv build/lite-paper/.venv
build/lite-paper/.venv/bin/pip install -r docs/lite-paper/tools/requirements.txt
build/lite-paper/.venv/bin/python docs/lite-paper/tools/build.py --lang zh-CN
```

The build writes `build/lite-paper/zh-CN/holon-lite-paper.pdf` and
`build-info.json`. It reads the Markdown at build time and records its hash, the
renderer hashes, font hashes, dependency version and output hash.

To generate page previews, install Poppler and ensure `pdftoppm` is on `PATH`:

```bash
build/lite-paper/.venv/bin/python docs/lite-paper/tools/build.py --lang zh-CN --preview
```

See [font setup](tools/fonts/README.md) for macOS fallback and portable build
configuration. No macOS font files are copied into the repository.

Review every page after each content/layout change. Check Chinese glyphs, table
breaks, text diagrams, links, source attribution and all product claims. Rendering
PNGs is not visual acceptance: `build-info.json` leaves `visual_review` pending.

## English version

Create `en.md` only when translation starts; no placeholder English download is
exposed. Use the same section order and stable metadata keys, and record the
Chinese source revision used for translation in `editorial.md`. Both locales
share `layout.py`, but can have different page counts. `--lang en` fails clearly
until an English source exists.

## Publish later

Once the content and rendered PDF are reviewed, copy the selected output to:

- `docs/website/assets/lite-paper/holon-lite-paper-zh-CN.pdf`
- `docs/website/assets/lite-paper/holon-lite-paper-en.pdf` (when ready)

These are stable download paths; version details stay inside the document. Track
approved PDFs in Git with their source changes. The build tool never publishes
and rejects output destinations under `docs/website/`.

Add the Chinese link to `docs/website/zh-CN/README.md` first, and the English link
to `docs/website/README.md` once ready. Verify the site bundle includes the PDF and
the homepage download works. `docs-site.yml` already watches `docs/website/**`;
normal site deployment then includes the reviewed asset. This source directory
does not itself trigger PDF publication.
