# Railway deployment

Holon can run as a regular Railway Service built from this repository's
`Dockerfile`. The checked-in [`railway.json`](../railway.json) supplies the
service build, `PORT`-aware start command, and `/settings` healthcheck.

## Service settings

Create an empty Service from this repository (or use the repository as the
source for a Template) and keep these settings:

| Setting | Value |
| --- | --- |
| Build | `Dockerfile` |
| Start command | `serve --listen 0.0.0.0:${PORT:-7878}` |
| Healthcheck path | `/settings` |
| Healthcheck timeout | `300` seconds |
| Public networking | Generate a Railway domain |

Railway injects `PORT`; the start command uses it instead of assuming the
local Compose port `7878`.

## Variables

Set these service variables:

| Variable | Value |
| --- | --- |
| `HOLON_HOME` | `/var/lib/holon` |
| `HOLON_WORKSPACE_DIR` | `/var/lib/holon/workspace` |
| `HOLON_BOOTSTRAP` | `1` |
| `HOLON_CONTROL_TOKEN` | A long random secret |
| `RAILWAY_RUN_UID` | `0` |

`HOLON_CONTROL_TOKEN` is required because Railway uses a non-loopback
listener. `RAILWAY_RUN_UID=0` is required for the current image because
Railway volume mounts are owned by root while the image normally runs as the
unprivileged `holon` user.

## Persistent storage

Attach one Railway Volume to the service at:

```text
/var/lib/holon
```

This configuration intentionally uses one volume mount and places the
workspace below `HOLON_HOME` for this deployment. The single mount persists
both the runtime state and `/var/lib/holon/workspace`.

## Template checklist

When creating a Railway Template, configure the service with the settings and
variables above, attach the volume at `/var/lib/holon`, and mark
`HOLON_CONTROL_TOKEN` as a generated secret or required secret. The first
browser visit to the generated domain should land on `/settings`, where a
provider credential and default model can be configured.

Railway Templates are composed and published in the Railway dashboard; this
repository provides the service-level `railway.json` and the exact Template
settings rather than a separate importable `template.json`.
