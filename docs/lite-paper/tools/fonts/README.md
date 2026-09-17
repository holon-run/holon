# PDF fonts

The renderer embeds the selected TrueType fonts in the PDF. Font files are not
vendored in this intermediate commit. Do not copy proprietary macOS fonts into
the repository.

Selection order:

1. `--font-regular` / `--font-bold` (also available through
   `HOLON_PAPER_FONT_REGULAR` / `HOLON_PAPER_FONT_BOLD`).
2. `Regular.ttf` / `Bold.ttf` in this directory, if installed locally.
3. macOS STHeiti Light/Medium as a local development fallback.

For Linux or consistent cross-platform builds, provide the same licensed,
Chinese-capable TrueType fonts on each machine. Static TrueType outlines are
required by the current ReportLab setup; arbitrary CFF OpenType or variable
fonts are not guaranteed to work. Explicit TTC files use face 0, except the
known macOS STHeiti collections which use face 1.

Before publishing, select and pin a redistributable font family, add its license
and attribution here, and review both languages using those exact font files.
Build metadata records the selected font file names and hashes.
