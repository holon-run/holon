# Holon lite paper

This directory owns the editable content and PDF build tools. Chinese and English editions are available. Each homepage links to the matching-language
PDF copy under website assets. The site deployment workflow publishes those
assets when the website changes are merged into the default branch.

## Files and ownership

| Path | Purpose | Commit? |
| --- | --- | --- |
| `zh-CN.md` | Chinese reader-facing copy; the PDF's content source | Yes |
| `en.md` | English reader-facing copy, localized from the Chinese edition | Yes |
| `editorial.md` | Open decisions, supporting evidence and editing notes | Yes |
| `assets/` | Shared diagram sources and necessary images | Yes |
| `tools/build.py` | Locale selection, source reading and build metadata | Yes |
| `tools/layout.py` | Shared typography and Markdown block layout | Yes |
| `tools/requirements.txt` | Pinned Python dependencies | Yes |
| `tools/fonts/` | Font setup; future licensed distribution fonts | Docs now |
| `../../build/lite-paper/<lang>/` | Review PDF, previews and build metadata | No |
| `../website/assets/lite-paper/` | Website copies of PDFs and localized GUI screenshots | Yes |

The previous v0.1/v0.3 PDFs, fixed-page scripts and diagnostics have been retained
locally under `build/lite-paper/history/`. The latest six-page script is also
preserved in `tools/archive/layout-v0.3.py` for its vector-layout implementation.
It is not a build entry point or active content source. Historical paths inside
archived scripts/notes are not maintained.

## Edit the content

Edit the language Markdown and rebuild that edition. Keep the two editions
aligned when changing product claims. Product copy is not duplicated in Python. Keep
internal decisions and unverified numerical claims in `editorial.md`.

The source begins with a small `key: value` metadata block delimited by `---`.
Supported fields are `lang`, `version`, `updated`, `status` and `title`; these are
plain strings, not a general YAML document. Filenames remain stable; revisions
belong in metadata and Git history.

The intermediate renderer supports the subset currently used by the paper:

- headings at levels 1–3, paragraphs, bold, emphasis and inline code;
- links, quotes, flat bullet lists and simple Markdown tables;
- local PNG/JPEG diagrams from `assets/`, with their editable sources beside them;
- fenced code and text diagrams.

Level-2 sections start on a fresh page after the opening section. Longer sections
can span pages; the renderer does not silently shorten the text to force six
pages. It uses shared typography and embeds the localized diagrams from
`assets/`. Edit paragraph length or the shared layout to tune pagination.

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
renderer hashes, font hashes, embedded image hashes, dependency version and output hash.

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

`en.md` follows the Chinese edition's section order and product claims, with
localized SVG / PNG diagrams and an English GUI capture. Both editions currently
have six pages and share `layout.py`. Translation provenance is in `editorial.md`.

For the current English preview on macOS:

```bash
python3 docs/lite-paper/tools/build.py --lang en --preview \
  --font-regular '/System/Library/Fonts/Supplemental/Arial.ttf' \
  --font-bold '/System/Library/Fonts/Supplemental/Arial Bold.ttf'
```

On other systems, supply suitable licensed TrueType fonts using the same flags.
Fonts are embedded in the PDF; the font source files are not copied into Git.

## Website downloads and local preview

Each language homepage links to its corresponding edition at a stable path:

- `docs/website/assets/lite-paper/holon-lite-paper-zh-CN.pdf`
- `docs/website/assets/lite-paper/holon-lite-paper-en.pdf`

After editing and reviewing a language, copy its generated PDF from
`build/lite-paper/<lang>/holon-lite-paper.pdf` to the corresponding website path.
Keep source and website copies in sync; verify that their SHA-256 hashes match.
The build command still rejects output paths under `docs/website/`, so this
copy remains an explicit step. `docs-site.yml` includes website assets in the
normal site deployment; adding files locally does not deploy the site.

Use the repository's locked mdorigin dependency (older global versions may not
support locale navigation):

```bash
npm ci --prefix docs/website/.tools
./docs/website/.tools/node_modules/.bin/mdorigin dev \
  --root docs/website --config docs/website/mdorigin.config.json --port 43210
```

Open `/zh-CN/` for Chinese or `/` for English. Check the language-matched PDF link on each homepage and narrow
screen layouts. Preview reports belong under `build/lite-paper/site-preview/`.
