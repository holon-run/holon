---
draft: true
---

# Assets

Static files used by the documentation shell. This directory is intentionally
excluded from mdorigin-managed indexes.

## WorkItem article diagrams

`../.tools/render-work-item-diagrams.py` generates the Chinese SVG/2x PNG
pairs. It requires CairoSVG and Noto Sans CJK SC.

- `work-item-architecture-zh`: 800×430, showing per-turn judgment, persistent
  work records, and resumption after waiting. It is not an internal component map.
- `work-item-reviewer-sequence-zh`: 800×438, showing one agent's execution line,
  external events above it, and retained waiting records below it. Dashed arrows
  show events satisfying resumption conditions, not immediate preemption.
- Both use a single desktop-oriented layout. Keep source dimensions
  synchronized with the article HTML.

Blue marks active processing, amber waiting, and green delivery; every state
also has a text label. The article's captions and alt text explain the flow
without relying on color. Keep text legible at article width when regenerating.

## Product article cover

`holon-agent-workspace-cover.webp` is an AI-generated conceptual illustration
selected by the operator for the Chinese product introduction on 2026-09-12.
It depicts Agent panels and task states, not a screenshot of the Holon UI.
The image is a full-frame 1672×941 WebP export of the second generated cover,
`holon-blog-agent-workspace-cover-v2.png`, with no cropping.

## Runtime architecture

`../.tools/render-runtime-diagram.py` generates both languages from shared
labels, relationships and layout rules. Run it without arguments to refresh all
four SVG/2x PNG pairs; `--language zh` or `--language en` limits the export.
It requires CairoSVG and Noto Sans CJK SC.

- `runtime-architecture-{zh,en}`: 1200×350 horizontal layout shared by the homepage
  and article bodies.
- `runtime-architecture-{zh,en}-narrow`: 480×550 vertical layout used only by the
  homepage picture element at viewport widths up to 1100px. Display at no more
  than 480px wide.

Keep HTML image dimensions and the homepage picture/CSS breakpoint synchronized.
The host boundary contains the runtime and execution resources, not interfaces;
it describes execution location, not a security sandbox.
Mobile uses a dashed outline and connector. Thin internal rules separate the
Holon heading and its three parallel responsibilities, without a workflow arrow.
The heading avoids runtime terminology; its subtitle describes ongoing work,
saved state and event wakeups. Technical terminology in article prose is unchanged.

The product article uses only `holon-tour-review-work.webp`, with a fixture-data
caption. Keep `holon-tour-agents.webp` and the versioned demo fixture available
for reuse; neither screenshot represents a live runtime work record.
