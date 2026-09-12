---
draft: true
---

# Assets

Static files used by the documentation shell. This directory is intentionally
excluded from mdorigin-managed indexes.

## Runtime architecture

`../.tools/render-runtime-diagram.py` generates both languages from shared
labels, relationships and layout rules. Run it without arguments to refresh all
four SVG/2x PNG pairs; `--language zh` or `--language en` limits the export.
It requires CairoSVG and Noto Sans CJK SC.

- `runtime-architecture-{zh,en}`: 1200×350 horizontal layout for wide homepages.
- `runtime-architecture-{zh,en}-narrow`: 480×550 vertical layout for articles and
  homepages at viewport widths up to 1100px. Display at no more than 480px wide.

Keep HTML image dimensions and the homepage picture/CSS breakpoint synchronized.
The host boundary contains the runtime and execution resources, not interfaces;
it describes execution location, not a security sandbox.
Mobile uses a dashed outline and connector. Thin internal rules separate the
Runtime heading and its three parallel responsibilities, without a workflow arrow.

The product article uses only `holon-tour-review-work.webp`, with a fixture-data
caption. Keep `holon-tour-agents.webp` and the versioned demo fixture available
for reuse; neither screenshot represents a live runtime work record.
