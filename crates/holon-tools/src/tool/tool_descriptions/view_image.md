Inspect a local image file (`path` + `prompt`), recording durable metadata (MIME, bytes, SHA-256, dimensions) and returning a cached or newly generated `visual_observation` with route-selection diagnostics.

Optional `model` (`provider@endpoint/model`) overrides the route for this call only without changing defaults or falling back; when omitted, `ViewImage` uses `vision.default` or auto-discovers an authenticated `image_input`-capable route (failing with `vision_adapter_unavailable` and candidate diagnostics if none is usable).
