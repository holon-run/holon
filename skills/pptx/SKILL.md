---
name: pptx
description: "Create, inspect, edit, render, and validate PowerPoint presentations with local open-source tools while preserving source files and reporting fidelity limits."
---

# PPTX

## Summary

Use this skill for `.pptx` presentation work. Prefer PptxGenJS for new,
layout-driven decks and `python-pptx` for inspection or bounded edits. Use an
available local LibreOffice or compatible renderer for visual validation.

This is a workflow and safety contract, not a presentation engine. Probe the
actual environment before choosing a backend.

## When To Use

- Creating a presentation from an outline, document, data set, or template
- Inspecting slide text, structure, notes, images, charts, or metadata
- Making bounded changes to an existing deck
- Rendering a deck to PDF or images for review
- Producing charts or tables from operator-provided data

## Safety Boundaries

- Keep the input unchanged and write a new `.pptx` by default.
- Treat hyperlinks, media, macros, embedded objects, and package metadata as
  untrusted input.
- Never execute VBA, embedded programs, OLE actions, or linked content.
- Ask before downloading linked assets or uploading deck content.
- Do not claim lossless editing when a deck contains animations, SmartArt,
  embedded media, custom XML, unsupported charts, signatures, or unknown parts.

For an existing deck, inventory those features before saving it. If the chosen
library cannot preserve important features, use read-only analysis, rebuild a
new deck, or request manual Office editing instead.

## Backend Selection

- Prefer **PptxGenJS** for new presentations requiring explicit placement,
  reusable layouts, shapes, images, tables, or charts.
- Prefer **python-pptx** for reading common slide content and small edits whose
  affected elements are represented by its object model.
- Use **LibreOffice headless** only when installed and approved as a local
  renderer or converter. Record that it is not Microsoft PowerPoint.
- Use local image tooling for montage or pixel inspection when available.

Do not silently install Node, Python packages, LibreOffice, fonts, or other
system dependencies. Report the missing capability and the safe fallback.

## Workflow

1. Confirm the audience, purpose, slide count, aspect ratio, language, branding,
   source data, output path, and whether a template must be preserved.
2. Inspect existing decks before editing. Record slide layouts, masters, fonts,
   charts, notes, media, links, and unsupported features that affect fidelity.
3. Create a content plan before implementation: one message per slide, evidence
   or data source, visual hierarchy, and speaker-note requirements.
4. Choose a backend and explain any expected limitations.
5. Generate or edit a new output file. Avoid raw OOXML manipulation unless the
   exact package contract is understood and no safer API can express the change.
6. Reopen the package and verify slide count, relationships, media references,
   and expected text or data.
7. Render every slide when a renderer is available. Inspect for overflow,
   clipping, overlap, unreadable charts, missing images, blank slides, and font
   substitution.
8. Sample-check numbers, labels, citations, and charts against source material.

## Quality Rules

- Keep a consistent grid, margins, typography, and color system.
- Prefer concise slide copy and meaningful visual structure over dense prose.
- Do not fabricate citations, figures, logos, or brand requirements.
- Preserve aspect ratio when placing images; flag low-resolution assets.
- Make charts readable without relying only on color.
- Use fonts known to exist in the target environment, or report substitutions.
- Separate speaker notes from visible slide content when notes are requested.

## Delivery

Report the output path, source preservation, backend and fonts used, rendered
pages inspected, structural checks, sampled data checks, and any unsupported or
unverified presentation features. If rendering was unavailable, say that visual
layout was not verified.
