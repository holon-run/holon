---
name: pdf
description: "Inspect, extract, assemble, generate, render, OCR, and validate PDF files with local tools while treating active content and external services as explicit security boundaries."
---

# PDF

## Summary

Use this skill for PDF reading, extraction, generation, page operations, forms,
rendering, and optional OCR. Match the tool to the operation instead of treating
PDF as a simple editable text format.

Common neutral backends include `pypdf` for page and metadata operations,
`pdfplumber` for text and table inspection, ReportLab or `pdf-lib` for
generation, Poppler for rendering, and qpdf for structural checks. OCR is a
separate, explicitly selected path such as local Tesseract.

## When To Use

- Extracting text, tables, metadata, links, bookmarks, or form information
- Splitting, merging, rotating, cropping, stamping, or encrypting documents
- Generating a PDF from structured content
- Rendering pages for visual review
- Running OCR on operator-approved scanned pages
- Comparing page-level content before and after a transformation

## Safety Boundaries

- Preserve the original and write transformed output to a new path.
- Treat JavaScript, actions, attachments, forms, links, signatures, and embedded
  files as untrusted.
- Never execute document JavaScript, launch actions, embedded programs, or
  external links.
- Do not remove passwords, permissions, signatures, or protection as a way to
  bypass access controls.
- External OCR or conversion requires explicit approval before upload.
- Warn that any content change can invalidate a digital signature.

If a parser reports corruption, suspicious object expansion, extreme page
dimensions, excessive object counts, or unsupported encryption, stop the
operation and report the limitation.

## Backend Selection

- Use **pypdf** for page assembly, rotation, cropping, metadata, common forms,
  and supported encryption operations.
- Use **pdfplumber** for positioned text and rule-based table extraction.
- Use **ReportLab** or **pdf-lib** for deliberate PDF generation and drawing.
- Use **Poppler** tools for local text extraction or rendering when installed.
- Use **qpdf** for structural validation and supported transformations.
- Use **Tesseract** only for explicitly requested local OCR and retain the
  original page images.

Do not describe text replacement as general PDF editing. Existing visual
content usually requires reconstruction, redaction, annotation, or a source
document change rather than in-place prose editing.

## Workflow

1. Confirm the requested pages, operation, output path, preservation needs,
   forms or bookmarks requirements, and whether OCR is allowed.
2. Identify the file type and inspect encryption, signatures, actions,
   attachments, links, page boxes, fonts, and page count.
3. Decide whether the task is extraction, page transformation, annotation,
   redaction, generation, or reconstruction.
4. Select the narrowest local backend that supports the operation.
5. Produce a new output without activating links or embedded content.
6. Reopen the result and check page count, dimensions, metadata, bookmarks,
   forms, attachments, encryption state, and expected text.
7. Render every changed page when possible and compare it with the intended
   visual result.
8. For extraction or OCR, sample the output against rendered pages and record
   ambiguous tables, reading order, missing glyphs, and low-confidence text.

## Quality Rules

- Preserve intended page order, orientation, crop boxes, and dimensions.
- Distinguish visual redaction from secure content removal; verify that redacted
  text and related objects are not extractable.
- Do not infer table structure without checking coordinates and rendered pages.
- Retain source page references in extracted content when useful.
- Label OCR-derived text and do not silently replace uncertain characters.
- Verify links, bookmarks, forms, and signatures if the requested operation can
  affect them.

## Delivery

Report the output path, page range and operation, tools used, whether OCR or any
network service was used, structural and visual checks, signature or form
impact, and extraction or OCR limitations. State explicitly when active content
was present but not executed.
