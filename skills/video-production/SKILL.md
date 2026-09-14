---
name: video-production
description: "Assemble existing local images, videos, audio and subtitles into preview/final videos with FFmpeg, technical QC and provenance. Use for local media production, not publishing or programmatic Remotion animation."
---

# Video Production

## Production contract

Before rendering, establish audience, script/storyboard, asset order and timing,
aspect ratio, resolution, audio/caption requirements and intended delivery.
Inventory local sources, ownership/license/consent, and missing assets. Do not
invent provenance or assume that a technical pass establishes usage rights.
Keep that editorial production checklist alongside the project.

Use the script below for straight-cut, 16:9 local assembly. For programmatic
animation use the separately installed official Remotion skill; this skill does
not install Remotion or provide a rendering backend. Generating images, video,
voice or TTS is an optional external capability: verify availability, consent,
cost and upload permission first. Missing generation capability is not a reason
to block assembly when local assets suffice. Never publish automatically.

## Local execution

Requires Python 3.10+, `ffmpeg` with libx264/AAC and `ffprobe` on PATH. Report
missing binaries/codecs explicitly; do not silently install, substitute a cloud
service, or claim an unavailable render succeeded.

Write a UTF-8 JSON manifest; relative paths resolve against its directory:

```json
{
  "clips": [
    {"path": "title.png", "kind": "image", "duration": 2},
    {"path": "demo.mp4", "kind": "video", "duration": 3}
  ],
  "audio": "narration.wav",
  "subtitles": "captions.srt"
}
```

Run from this skill directory (or use an absolute script path):

```bash
python3 scripts/produce.py /project/manifest.json /project/preview-v1 --mode preview
python3 scripts/produce.py /project/manifest.json /project/final-v1 --mode final
```

Output directories must be new. Original assets are not overwritten. The helper
accepts PNG/JPEG stills, MP4/MOV videos, optional WAV/MP3/M4A audio, and UTF-8 SRT
captions only. Unknown manifest fields and unsupported extensions are errors.
Use ordinary standalone local media, not playlists or externally linked media.
Do not process untrusted media without an appropriately isolated environment;
this helper is not a sandbox.

Clips start at time zero, cut at their requested positive duration, and are
scaled/padded (not cropped) to 640×360 preview or 1280×720 final at 25 fps,
H.264/yuv420p. Each duration is rounded to the nearest 25 fps frame (ties up,
minimum one frame); audio/caption validation and muxing use that same timeline,
recorded in the report. Source clip audio is explicitly
removed; the single optional audio track replaces it, must cover the whole
timeline, and is trimmed to length and encoded to AAC. There is no mixing,
transition, animation, trim-offset or portrait support. Do not silently convert
an unsupported request into this contract: adapt the workflow explicitly.
SRT cues must be ordered, nonoverlapping and within the timeline; captions are
selectable MP4 subtitles, not burned in. Confirm player/platform compatibility.

## Review and delivery

Each output directory contains the MP4 and `report.json`: source paths/SHA-256,
manifest, tool versions, output hash, ffprobe metadata and full audio/video
decode result. Hash provenance identifies files; add original source URLs,
licenses and generation details to the production checklist when known.

Inspect the preview visually and audibly: framing, pacing, text legibility,
caption timing, pronunciation, transitions, silence and clipping. Technical QC
checks streams, dimensions/pixel format, duration and complete decoding; it does
not prove editorial quality, loudness compliance, accessibility or rights.
Resolve issues before final rendering and inspect the final too. Deliver source
manifest/checklist, preview/final and QC report, noting limitations and any
unverified requirement. Publication requires separate operator authorization.

Offline fixture verification (creates and cleans temporary assets):

```bash
python3 -m unittest discover -s skills/video-production/tests -v
```

Run that test command from the repository root.
