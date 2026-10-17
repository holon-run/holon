---
name: 3d-production-workflow
description: "Turn a creative brief into shared assets, editable scenes and verified 3D games or animated films. Use for cross-stage production, asset reuse and multi-view correction, not engine API reference or ordinary video editing."
---

# 3D Production Workflow

Move from a generated model to a playable work or deliverable film. Use the
project's chosen engine and technical skills; this workflow does not install
tools, grant spending permission or establish production readiness.

## Decompose the work before generating assets

- List subjects, environment, interactions, cameras and motion from the target
  frame or gameplay. With reference images, distinguish occlusion, perspective,
  silhouette and lighting; a hidden back surface is unknown, not established
  geometry.
- Keep a small production manifest: stable asset ID, purpose, reuse,
  provenance/license status, creation method, scale/axes, material style,
  performance limits and acceptance viewpoints.
- Prove space, camera framing, reachability and gameplay with a graybox first.
  Prefer procedural geometry for simple platforms, crystals and collision
  shapes. Consider generated/external assets when complex silhouettes matter
  to recognition, not for every object.
- If references are absent, make a composition sketch or graybox and record
  design choices. Do not buy or upload material merely to satisfy a reference
  step.
- Test the delivery path with a minimal sample before expensive production:
  authentication, accepted formats/MIME, CSP, size limits, playback/download.
  Local rendering success is not hosted delivery. If unsupported, preserve the
  original files and report local-file delivery limits; do not bypass login,
  modify the host without approval, or call a frame preview MP4 playback.

## Generation is not assembly

- Confirm model/quality, expected cost, task count and retry limits before
  generation. Without spending approval, continue only with grayboxing and
  free steps.
- For optional Tripo, use the configured UXC interface and local connection
  instructions. Discover operations, inspect schemas and verify current
  official pricing; do not guess parameters from tutorials or mix API
  versions. Require both UXC `ok == true` and Tripo `data.code == 0`.
- Distinguish request acceptance, remote terminal success, download and scene
  import. Record the task ID before waiting. After uncertain submission,
  reconcile known tasks/usage before retrying a chargeable request.
- Preserve original models and reproducible import settings. Record provenance,
  generation parameters, actual credits and license status, not keys or signed
  download URLs. Downloading from a third-party CDN must not forward the
  service API key.
- Inspect readable meshes/textures, bounds, units, up/forward axes, origin,
  color space and triangle count. Normalize scale/orientation in an explicit
  scene adapter rather than repeatedly destructively editing the source.

## Share scenes and correct multiple views

- Reuse source assets through the manifest and rebuildable scene modules, not
  divergent game/film copies. Separate collision and navigation/reachable areas
  from visual geometry.
- Establish subject scale/style before assembling environment, materials,
  lighting and cameras. Revisit the original intent each iteration rather
  than optimizing a single attractive viewpoint.
- Check front/side/back or overhead views plus the actual output camera for
  silhouette, occlusion, proportion, intended floating/ground contact and
  intersections. Use real rendered screenshots; do not claim visual-model
  review unless it actually ran.
- For characters only when needed: multi-view consistency, mesh, rig/skin,
  motion adaptation, then contact/intersection checks. Skip this branch for
  non-character or unrigged work. A generated model does not prove usable
  character animation.

## Validate each requested output

- **Game:** deliver source and a playable build with controls, objective and
  applicable win/loss/restart behavior. Run real builds and browser/WebGL
  checks for input, collision/collection, end/restart states, console and asset
  requests. A film cannot substitute for playable acceptance.
- **Film:** use its own camera/timeline, deterministic fixed-timestep frame
  rendering and video encoding. Deliver editable scenes, render/encode
  entrypoints and the film. Check dimensions, frame rate, frame count/duration
  with `ffprobe` and inspect first/middle/last and key-action frames. An orbit
  preview or screen recording does not substitute for editable source and the
  agreed rendered film.
- Keep build/run/render commands, lockfiles, asset manifest, actual cost and
  evidence. Separate assertions, screenshot checks and visual review; name the
  tested device/browser and untested delivery paths. If import fails, a graybox
  may validate other stages, but report the generated-asset path as incomplete.
- Rebuild every affected output after shared-asset edits. Improve this workflow
  from observed failures rather than adding speculative instructions or
  generalizing one successful trial into a production-quality claim.

## Method provenance

Inspired by [Saccc_c's 3D production article](https://x.com/Saccc_c/status/2110694725640474759):
reference decomposition, differentiated asset creation, scene assembly,
multi-view correction and an optional character branch. This is an original
workflow synthesis, not a reproduction or endorsement. Game/film acceptance and
cost controls are engineering additions, not claims about the article. A
reference does not grant asset rights or execution authority.
