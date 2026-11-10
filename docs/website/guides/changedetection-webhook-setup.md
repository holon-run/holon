---
title: Wake Holon Agents on Web Page Changes with Self-Hosted changedetection.io
summary: "Monitor web pages with self-hosted changedetection.io and wake Holon agents via webhook callbacks when changes are detected: Docker deployment, watch and notification configuration, and end-to-end verification."
order: 189
---

# Wake Holon Agents on Web Page Changes with Self-Hosted changedetection.io

changedetection.io is an open-source (Apache-2.0) web page change detection and monitoring tool: you configure a target URL, and when content changes, it alerts you via notification channels including custom webhooks. By connecting it to a Holon agent's external callback ingress, a detected web page change becomes a wake event. The awakened agent can then inspect the updated content, update its tasks, or act according to predefined rules. This guide walks through the complete workflow from deployment to end-to-end verification, fully reproducible on a single machine without third-party cloud accounts.

This scenario implements Holon's [Connect external events](./connect-external-events.md) workflow: the webhook serves as a wake signal (`wake_hint`). The agent resumes, independently verifies external state, and decides its next actions. Note that changedetection.io 0.70+ also introduces a native MCP server interface for tool-plane integration, which is outside the scope of this guide.

## Prerequisites

- A host capable of running Docker.
- Holon 0.59.0 or higher (with support for external callback ingress `POST /api/callbacks/wake/:token` and `holon agent reset-callback`).
- changedetection.io 0.70.3 (verified in November 2026).

## Step 1: Deploy changedetection.io

```bash
docker run -d --name changedetection \
  --network host \
  -p 127.0.0.1:5000:5000 \
  -v "$PWD/cdet-data:/datastore" \
  dgtlmoon/changedetection.io:0.70.3
```

Two critical operational details verified in testing:

- **The container volume mount point must be `/datastore`, not `/data`**. In the 0.70.3 image, all watch definitions and snapshots live in `/datastore` (each watch under a `<uuid>/watch.json` directory, plus a global `changedetection.json`). Mounting to `/data` produces no warning or error, but all configurations are silently lost upon container restart.
- If you plan to monitor **loopback or private network addresses** (such as internal services on the same host), add `-e ALLOW_IANA_RESTRICTED_ADDRESSES=true`. This controls changedetection.io's built-in SSRF protection (GHSA-rph4-96w6-q594), which by default blocks loopback and private IP ranges to prevent internal port scanning. Enable this flag only in isolated testing environments; keep it disabled in production.

Once started, open `http://<host>:5000` to access the web UI. Authentication is disabled by default (passwords can be configured via environment variables), and all management actions can also be driven via the REST API (`/api/v1/...` after generating an API key in Settings).

## Step 2: Prepare a target watch

This walkthrough demonstrates change detection using a local static HTTP server. In production, this can be any accessible endpoint: documentation pages, release notes, pricing matrices, or issue lists.

```bash
mkdir -p /srv/watch-target
printf '<html><body><h1>status page v1</h1></body></html>\n' > /srv/watch-target/page.html
python3 -m http.server 8088 --directory /srv/watch-target --bind 127.0.0.1 &
```

In the changedetection.io web UI, click **Add a watch**, enter `http://127.0.0.1:8088/page.html`, and save. It immediately fetches an initial snapshot as the baseline. The default check interval is 3 hours; for testing, trigger an immediate recheck via **Check now** in the watch list or via `POST /checknow?uuid=<watch-uuid>`.

## Step 3: Obtain the Holon callback capability

Every Holon agent has a default external callback ingress. For isolated experiments, issue a dedicated callback for a disposable test agent (`reset-callback` revokes any prior URL and returns a new token):

```bash
holon agent reset-callback --agent <agent-id> > callback.json
chmod 600 callback.json
jq -r '.delivery_mode' callback.json   # Expected: wake_hint
jq -r '.trigger_url' callback.json     # The secret callback URL; treat as confidential
```

The callback URL is a capability secret: anyone with the URL can wake the agent. It should reside solely in the sender's configuration (changedetection.io). Throughout this document, `<wake-url>` is used as a placeholder.

Inside the agent, register an external wait before pausing (this is an agent tool call, not an HTTP setting):

```json
{"wake": "external", "resource": "cdet:watch:<watch-uuid>", "reason": "Check latest content after page change", "delivery": "silent"}
```

## Step 4: Configure webhook notifications on the watch

Edit the watch and configure the **Notifications** section:

| Field | Value | Notes |
|---|---|---|
| Notification URL | `json://127.0.0.1:7878/api/callbacks/wake/<token>` | Apprise JSON channel: replace `http://` with `json://` from `<wake-url>`. Apprise sends an HTTP POST with a JSON payload |
| Notification Title | `changedetection: {{watch_title}} changed` | Variables are rendered by Apprise |
| Notification Body | `watch {{watch_url}} detected a change; event id {{watch_uuid}}` | Sent inside the JSON payload |
| Notification Format | `text` | **Must be strictly lowercase**; entering `Text` causes Apprise to fail with `Invalid notification format: "Text"`, silently aborting delivery |

Valid notification formats are `text`, `html`, `htmlcolor`, `markdown`, and `System default` (which defaults to `htmlcolor`). The `json://` channel dispatches a JSON body structured like:

```json
{"version": "1.0", "title": "...", "message": "...", "attachments": [], "type": "info"}
```

## Step 5: End-to-end verification

1. Update the target page content (v1 → v2) and click **Check now**.
2. The changedetection.io container log should show `Change detected in UUID <uuid>` (typically <1s latency in local tests), followed by `Notifying 1 service(s)` and the outgoing `JSON POST URL: ... /api/callbacks/wake/...`.
3. The agent is resumed: running `holon tail --agent <agent-id> --include-progress` displays the new turn and "external wake arrived" status. The agent inspects the target page, confirms the new content, and delivers its conclusion.

Negative test results verified on changedetection.io 0.70.3 × Holon 0.59.0 (November 2026):

| Case | Behavior |
|---|---|
| Invalid callback token | Holon returns HTTP 403 with `{"code":"forbidden","error":"invalid callback token","ok":false}`; agent is not awakened |
| Target returns 404 | Not treated as a diff; no notification is triggered (`checksum ... the same, nothing todo`), avoiding false alarms |
| Notification endpoint timeout | Apprise logs `Connection error ... Read timed out`; watch status reflects the failed dispatch |

## Known issue: wake callback response delay

Testing revealed that Holon's `POST /api/callbacks/wake/:token` endpoint may not promptly return an HTTP response to valid tokens (senders waiting for 4s or 30s timeouts receive no ack). While the event is successfully recorded and the agent eventually wakes, response latency can reach several minutes. See [holon-run/holon#4016](https://github.com/holon-run/holon/issues/4016) for details. Until resolved:

- changedetection.io (Apprise defaults to a 4s timeout) will log each dispatch as failed, even though the wake delivery succeeds. Do not reconfigure or cycle URLs based on this error alone.
- To reduce log noise, append timeout parameters supported by Apprise to the notification URL.
- Always verify delivery through agent tail and execution status rather than HTTP client status codes.

## Troubleshooting

| Symptom | Cause and remedy |
|---|---|
| Watches disappear after container restart | Volume mounted to `/data`; change mount to `/datastore` |
| Adding watch returns HTTP 400 in API automation | Form requires CSRF token: GET the page first to capture `csrf_token` and session cookie before sending the POST |
| Target URL rejected or unreachable | SSRF protection active; add `ALLOW_IANA_RESTRICTED_ADDRESSES=true` for local testing |
| Notification reports `Invalid notification format: "Text"` | Format parameter must be lowercase (e.g., `text`, `html`, `markdown`) |
| Notification logs `Read timed out` but agent resumes | Known behavior in Holon #4016; see Known Issue above |
| Agent does not resume | Ensure agent registered `WaitFor(wake=external)` and is not stopped. Stopping an agent automatically revokes all its external triggers |

## Cleanup

After verification: remove the watch and container (`docker rm -f changedetection` and volume directories), and terminate test server processes. Resetting callbacks revokes capabilities on stopped agents; ensure temporary files such as `callback.json` containing unmasked URLs are securely deleted.
