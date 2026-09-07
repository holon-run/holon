---
name: docx
description: "Create, inspect, edit, render, and validate Word documents with local open-source tools while preserving source files and making unsupported OOXML features explicit."
---

# DOCX

## Summary

Use this skill for `.docx` reports, memos, letters, meeting notes, proposals,
and other word-processing documents. Prefer `python-docx` for common document
structures and use an available local LibreOffice renderer for pagination and
visual review.

This skill does not imply complete support for every Word or OOXML feature.

## When To Use

- Creating a structured document from notes, Markdown, or source material
- Inspecting paragraphs, headings, tables, images, styles, and metadata
- Making bounded edits to a conventional DOCX
- Converting a DOCX to PDF for review
- Checking document structure, pagination, and consistency

## Safety Boundaries

- Preserve the source and write a new `.docx` by default.
- Treat macros, links, fields, embedded files, OLE objects, comments, revisions,
  and custom XML as untrusted input.
- Never execute macros, update external fields, open embedded objects, or follow
  document links automatically.
- Ask before resolving external assets or uploading content.
- Do not claim lossless editing for content controls, complex fields, tracked
  changes, equations, signatures, embedded objects, or package parts not modeled
  by the selected library.

Use read-only analysis or create a new document when preserving unsupported
features is material to the request.

## Backend Selection

- Use **python-docx** for common paragraphs, styles, lists, sections, tables,
  headers, footers, and images.
- Use **LibreOffice headless** only when installed and approved as a local
  renderer or converter. Record the actual backend.
- Use package-level XML inspection for risk inventory or verification, not as a
  default editing strategy.

Do not silently install Python packages, fonts, LibreOffice, or converters.

## Workflow

1. Confirm the document type, audience, language, page or word target, style
   guide, template, citations, output path, and whether comments or revisions
   must be preserved.
2. Inspect an existing package before editing. Record styles, sections, headers
   and footers, tables, fields, links, comments, revisions, macros, signatures,
   embedded objects, and unknown parts.
3. Plan the heading hierarchy, sections, tables, figures, references, and page
   furniture before generation.
4. Select a backend and restrict changes to features it represents reliably.
5. Write a new output file. Avoid raw OOXML edits unless the relationship and
   namespace effects are fully understood.
6. Reopen the result and verify expected headings, paragraph and table counts,
   relationships, images, sections, headers, and footers.
7. Render to PDF or pages when possible. Inspect page breaks, widows and orphans,
   clipped tables, heading placement, list numbering, headers, footers, and font
   substitution.
8. Sample-check important facts, citations, figures, and tables against source
   material.

## Quality Rules

- Use semantic styles rather than formatting every paragraph independently.
- Maintain a coherent heading hierarchy and consistent spacing.
- Keep tables within page bounds and repeat header rows when appropriate.
- Do not fabricate quotations, citations, legal language, or factual claims.
- Record unresolved comments, revisions, fields, or links that remain.
- Treat automatic tables of contents and fields as needing an actual update
  backend; writing field instructions alone does not prove displayed values.

## Delivery

Report the output path, whether the source was unchanged, backend and fonts
used, structural and rendered checks, citation or data sampling, and any impact
on comments, revisions, fields, links, macros, signatures, or unsupported OOXML
features. If rendering was unavailable, state that pagination was not verified.
