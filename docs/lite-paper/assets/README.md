# Lite paper assets

Keep editable diagram sources and document images here. Page previews, PDF
outputs and rendering diagnostics belong under `build/lite-paper/`.

## Localized diagram set

| SVG / PNG basename | Purpose |
| --- | --- |
| `work-loop-zh-CN` | Agent work loop, waiting/resumption and delivery within Holon |
| `pr-review-zh-CN` | Successive events resume the same PR work item |
| `multi-workspace-zh-CN` | Reserved for future documentation; not embedded in the current paper |
| `team-handoff-zh-CN` | Shared records, shared Agents and human responsibilities across handoffs |

Each SVG is the authoritative source for its localized labels and layout. Each
PNG is a generated 1.5x rendition (2700 × 1620). The current `../zh-CN.md`
embeds the work-loop, PR-review and team-handoff renditions for PDF rendering.
The multi-workspace pair is retained as reusable documentation material. Track both; these PNGs are document images, not page
previews. All SVG canvases are 1800 × 1080.

Regenerate all Chinese PNGs from the repository root using librsvg:

```bash
for source in docs/lite-paper/assets/*-zh-CN.svg; do
  rsvg-convert --zoom 1.5 --output "${source%.svg}.png" "$source"
done
```

The SVGs use Chinese font fallbacks; ensure a suitable font is installed before
rendering, then inspect every changed PNG for overflow and crossed labels. The
checked-in renditions fix the reviewed appearance for downstream PDF builds.
English counterparts use the same basenames with `-en` in place of `-zh-CN`.
Their SVGs contain localized labels fitted to the same visual structures.
Regenerate them with `rsvg-convert --zoom 1.5` as above.

Edit labels in the SVG, regenerate its PNG, and update the related explanation
in the language Markdown together. Do not duplicate diagram copy in Python.
The opening figure explains the work loop; the two scenario figures explain
event continuity and team handoffs. The reserved multi-workspace figure explains
execution environments and supervised child tasks.

## Current GUI screenshot

`web-gui-review-zh-CN.png` is an unedited screenshot of the real Chinese GUI,
using synthetic reviewer data. It is embedded in section 5. Its adjacent
`web-gui-review-zh-CN.capture.json` records the GUI version and provenance.
The screenshot is not generated from SVG. See
[`../tools/gui-tour/README.md`](../tools/gui-tour/README.md) for scene data,
capture instructions and limitations. The Markdown caption must identify demo
data; this image does not attest to a real review or CI run.

## English GUI screenshot

`web-gui-review-en.png` is a real English GUI capture with localized synthetic
scenario data. Its adjacent `.capture.json` records the GUI commit and hashes.
The English capture uses commit `7b96b94a89b8ef1e67a96c8d349c60eee2eb4b99`;
the Chinese capture retains its previously reviewed commit. Neither screenshot
is evidence of a live review or CI run.
