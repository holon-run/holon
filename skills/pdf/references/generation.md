# PDF Generation

Use this workflow whenever the task creates or reconstructs a PDF. The goal is
not merely a valid file: the document must remain readable, visually coherent,
and portable on a machine that does not share the authoring environment.

## 1. Choose the Rendering Model

Choose the backend from the document's structure:

- **HTML/CSS to PDF**: default for reports, proposals, guides, newsletters,
  resumes, and mixed text/table/image documents. Browser layout handles flowing
  prose, wrapping, spacing, and page breaks better than manual coordinates.
- **ReportLab or pdf-lib**: use for labels, certificates, forms, overlays,
  diagrams, fixed-position one-pagers, or drawing-heavy output.
- **Source application export**: when an editable DOCX, PPTX, or spreadsheet is
  the real source of truth, generate that source first and export a PDF locally.

Do not choose a coordinate-driven library merely because it can emit PDF. It
often produces brittle wrapping, inconsistent vertical rhythm, and unattractive
default typography for long documents.

## 2. Establish a Small Design System

Before implementing pages, write down:

- document class and audience
- page size and orientation
- body and display font families
- body size and leading
- heading scale
- page margins and content width
- primary text, muted text, border, surface, and accent colors
- recurring components such as cover, summary, callout, table, figure, and
  footer

For a neutral A4 Chinese business report, use these starting ranges:

| Element | Starting value |
| --- | --- |
| Page margins | 16–22 mm |
| Body | 10–11 pt |
| Body line height | 1.55–1.75 |
| H1 | 24–30 pt |
| H2 | 16–19 pt |
| H3 | 12–14 pt |
| Caption / metadata | 8–9 pt |
| Paragraph measure | roughly 32–45 full-width Chinese characters |

Use one main font family and at most one contrasting display family. Use one
accent hue plus neutral colors unless the operator provides brand tokens.
Prefer whitespace, alignment, hierarchy, and rules over decorative gradients,
heavy shadows, or many unrelated colors.

The bundled `../assets/chinese-report.css` provides a restrained default for
Chinese and mixed Chinese/Latin reports. Copy it into the task's working
directory and customize its tokens and components for the requested identity.

## 3. Select and Verify Chinese Fonts

### Discovery

Never assume that a font name exists. Inspect the local system:

```bash
fc-match "Noto Sans CJK SC"
fc-match "Noto Serif CJK SC"
fc-list :lang=zh family file | head -40
```

Reasonable open-font preferences include Noto Sans/Serif CJK SC and Source Han
Sans/Serif CN. Platform fonts such as PingFang SC or Microsoft YaHei may be
appropriate when locally installed and redistribution is not required.

Use the Simplified Chinese (`SC`) face for mainland Simplified Chinese unless
the task requires Traditional Chinese, Japanese, or Korean glyph conventions.
Do not silently use a Japanese CJK face for Chinese merely because it contains
the characters.

### Embedding

- HTML/browser generation may subset and embed the selected local font. Verify
  the result rather than assuming it did.
- For portable output, use an operator-approved font file with a license that
  permits embedding. A CSS `@font-face` rule is more deterministic than a long
  system fallback chain.
- ReportLab `TTFont` requires a supported TrueType-outline font. Many CJK
  OTF/TTC packages, including some Noto CJK installations, use PostScript/CFF
  outlines and fail even when `subfontIndex` is correct. Test registration
  before building the document. For a supported TTC collection, inspect and
  select the correct `subfontIndex` instead of copying an index from another
  machine. If the selected font is unsupported, choose a suitable
  TrueType-outline font or switch to the HTML/CSS path; do not silently fall
  back to Helvetica.
- Avoid non-embedded viewer-dependent CJK fonts for deliverables that must render
  consistently elsewhere.

Inspect the finished document:

```bash
pdffonts output.pdf
pdftotext output.pdf - | sed -n '1,80p'
```

For every font used for visible content, check that `emb` is `yes` where the
backend supports embedding. Extracted text should preserve representative
Chinese punctuation, numerals, Latin abbreviations, and uncommon characters.
Some browser/PDF combinations expose embedded CJK glyphs as anonymous Type 3
fonts. In that case, `emb=yes` alone is insufficient: verify rendered glyph
quality and text extraction, and change the backend if named/searchable font
behavior is a delivery requirement.

## 4. Compose Pages Deliberately

- Give the first page a clear entry point: title, concise subtitle, ownership or
  date metadata, and enough whitespace.
- Balance display-heading line breaks. Never leave one or two Chinese
  characters isolated on a title line; use semantic manual breaks when the
  renderer's balancing is insufficient.
- Keep headings with the following paragraph. Avoid a heading alone at the
  bottom of a page.
- Use paragraph spacing or first-line indentation consistently, not both by
  accident.
- Keep body text comfortably dense. Do not shrink text to rescue an overloaded
  page; edit content, restructure sections, or add a page.
- Use tables for comparison, not general page layout. Repeat headers when the
  backend supports it and avoid overly narrow columns.
- Keep figures with their captions and preserve image aspect ratios. Use
  sufficient source resolution for the final physical size.
- Use page breaks intentionally before major sections, but avoid sparse pages
  caused by unnecessary forced breaks.
- Make links visibly distinct without printing raw, distracting URLs unless the
  document needs them.

## 5. Render and Review

For local Chrome:

```bash
google-chrome \
  --headless --disable-gpu --no-pdf-header-footer \
  --allow-file-access-from-files \
  --print-to-pdf=output.pdf input.html
```

Some container environments also require `--no-sandbox`; add it only when the
local execution environment requires it.

Render every page to images:

```bash
mkdir -p rendered
pdftoppm -png -r 144 output.pdf rendered/page
```

Inspect all rendered pages at readable scale. A contact sheet is useful for
rhythm and consistency, but inspect individual pages for text defects.

Revise when any of these appear:

- missing-glyph boxes or visibly wrong regional glyph forms
- unexpected font substitution or unembedded primary fonts
- clipped, overlapping, or off-page content
- one- or two-character title lines and very short paragraph spill lines
- headings stranded at page bottoms
- isolated final lines or sparse spill pages
- inconsistent margins, heading spacing, or component styles
- weak hierarchy, low contrast, excessive decoration, or monotonous walls of
  text
- tables whose cells wrap into unreadable fragments
- low-resolution or stretched images

Run structural and text checks again after the final visual revision.

## 6. Delivery Evidence

Report:

- rendering backend and relevant conversion command
- page size and page count
- primary font family and embedding result
- whether all pages were rendered and visually inspected
- any substitutions, unsupported typography, or portability limitations

Do not claim the document is visually verified when only the source or extracted
text was inspected.
