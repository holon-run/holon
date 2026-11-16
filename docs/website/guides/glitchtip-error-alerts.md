---
title: Wake Holon Agents on Error Alerts with Self-Hosted GlitchTip
summary: "Self-hosted GlitchTip as error tracking: errors ingested via Sentry-compatible endpoints generate issues, project alert webhooks wake Holon, and agents verify issues and events via REST APIs without modifying Holon runtime code."
order: 189
---

# Wake Holon Agents on Error Alerts with Self-Hosted GlitchTip

GlitchTip is an open-source, Sentry-compatible error tracking platform (both backend and frontend repositories are MIT licensed). Holon requires no dedicated GlitchTip adapter: a project alert's generic webhook recipient posts an error summary to Holon's wake callback, and the agent uses a project API token to retrieve issue and event details.

This guide covers only this integration path. It is not an OIDC integration and does not replace client-side application SDK ingestion.

## Verified Versions and Findings

Verified on 2026-11-16 in an isolated Docker Compose environment. **No Holon runtime code modifications are required.**

- GlitchTip container image `glitchtip/glitchtip:latest`, digest `sha256:c245745b4179667da3aebd73fb8d4f1fd918f7d68ba07d1fd73ea688933b6e01` (built 2026-11-09). Inbound delivery requests use User-Agent `GlitchTip/7.0.0`.
- Holon `0.59.0` (`33e2fcb4b`).
- Forward flow: After ingesting an error via `POST /api/{project_id}/store/?sentry_key=<DSN public key>`, the webhook arrives within approximately one alert cycle. Holon responds to `POST /api/callbacks/wake/<token>` with HTTP 200. The REST response for `GET /api/0/issues/1/` has a `title` that matches the webhook `attachments[0].title` (both `ProbeError: glitchtip webhook verification` in this test). `GET /api/0/issues/1/events/` returns an `event_id` matching the ingestion response.
- Negative cases: Missing or invalid Bearer tokens return `401 {"detail":"Unauthorized"}`. When `quantity` exceeds the number of events in the time window, no webhook is dispatched and no wake occurs. An invalid recipient URL will not reach Holon.

## Prerequisites

- A machine capable of running Docker. Bind GlitchTip only to localhost or a private test network; do not expose port 8080 to the public internet.
- An existing Holon `wake_hint` callback URL. This URL is a capability secret: enter it only into GlitchTip's recipient configuration; do not write it into documentation, plans, logs, or screenshots.
- If the callback host is on a local, loopback, or Tailscale CGNAT address (`100.64.0.0/10`), you must enable `GLITCHTIP_ALLOW_PRIVATE_IPS`. GlitchTip treats CGNAT ranges as non-routable private addresses; if disabled, recipient creation will be rejected or dropped by outbound SSRF protections.

## Step 1: Deploy GlitchTip

The current `latest` image uses a single-process architecture: `./bin/start.sh` serves HTTP, consumes background task queues, and initializes the database on startup. Do not split `migrate` or `worker` services as older documentation suggested; running `bin/run-worker.sh` in the image will be explicitly rejected.

Queue configuration uses `VALKEY_URL`; `REDIS_URL` serves as a fallback. `redis:7` protocol is fully compatible. The HTTP listening port is controlled by `PORT`, mapped below to localhost `127.0.0.1:8080`.

```yaml
services:
  postgres:
    image: postgres:16-alpine
    environment:
      POSTGRES_DB: glitchtip
      POSTGRES_USER: glitchtip
      POSTGRES_PASSWORD: "<db-password>"
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U glitchtip -d glitchtip"]
      interval: 5s
      retries: 20
  redis:
    image: redis:7-alpine
  web:
    image: glitchtip/glitchtip:latest
    ports:
      - "127.0.0.1:8080:8080"
    environment:
      DATABASE_URL: "postgres://glitchtip:<db-password>@postgres:5432/glitchtip"
      VALKEY_URL: "redis://redis:6379/0"
      SECRET_KEY: "<random-secret-key>"
      PORT: "8080"
      GLITCHTIP_DOMAIN: "http://glitchtip.example.com:8080"
      ENABLE_ORGANIZATION_CREATION: "true"
      GLITCHTIP_ALLOW_PRIVATE_IPS: "true"
    depends_on:
      postgres:
        condition: service_healthy
volumes:
  pgdata:
```

`GLITCHTIP_DOMAIN` must be the final address accessed by users and agents. If omitted, the webhook's `title_link` and API `permalink` default to `http://localhost:8000/...`. Keep `GLITCHTIP_ALLOW_PRIVATE_IPS=true` only when the callback target resides on an internal network.

Health check: `GET /_health/` returns HTTP 200.

## Step 2: Create User, Project, and API Token

The current image's `HEADLESS_CLIENTS` setting only supports `browser`; there is no app token client. Registration uses the headless browser API and requires obtaining a CSRF cookie first:

1. `GET /_allauth/browser/v1/config`
2. `POST /_allauth/browser/v1/auth/signup` with JSON `{"email","password"}` and headers `X-CSRFToken` and `Referer`. When email delivery is unconfigured, `ACCOUNT_EMAIL_VERIFICATION` defaults to `none`, immediately logging in and issuing a `sessionid`.
3. Using the same session: `POST /api/0/organizations/` `{"name":"..."}`, `POST /api/0/organizations/{org}/teams/` `{"slug":"..."}`, and `POST /api/0/teams/{org}/{team}/projects/` `{"name":"...","platform":"python"}`.
4. `POST /api/0/api-tokens/` with session authentication, body example: `{"label":"holon","scopes":["project:read","project:write","event:read","event:write","org:read"]}`. The returned `token` appears only once; treat it as a secret.
5. `GET /api/0/projects/{org}/{project}/keys/` to read the DSN. The `public` field is a UUID; strip its hyphens to use as `sentry_key` during ingestion.

Subsequent queries use `Authorization: Bearer <token>` without depending on the session cookie.

## Step 3: Configure Alert Recipient

`POST /api/0/projects/{org}/{project}/alerts/`:

```json
{
  "name": "holon-error-alert",
  "timespanMinutes": 60,
  "quantity": 1,
  "uptime": false,
  "alertRecipients": [
    {"recipientType": "webhook", "url": "http://<holon-host>:<port>/api/callbacks/wake/<token>"}
  ]
}
```

`recipientType` must be `webhook` (generic webhook). `quantity` specifies the error threshold, and `timespanMinutes` defines the evaluation window. Both fields must be set, or the alert will be skipped by background evaluation tasks. The default evaluation interval is `ALERT_NOTIFICATION_INTERVAL` seconds (defaults to 60 if unset).

## Step 4: Ingest a Test Error

```bash
curl -sS -X POST "http://127.0.0.1:8080/api/1/store/?sentry_key=<dsn-public-key>"   -H 'Content-Type: application/json'   -d '{"event_id":"<uuid>","message":"probe","level":"error","platform":"python","exception":{"values":[{"type":"ProbeError","value":"glitchtip webhook verification"}]}}'
```

A successful ingestion returns `{"event_id":"..."}`. The issue is generated asynchronously by worker tasks, and the alert is dispatched in the next evaluation cycle.

## Webhook Payload and API Mapping

The generic webhook delivers a summary rather than the raw Sentry event:

```json
{
  "text": "GlitchTip Alert",
  "attachments": [
    {
      "title": "ProbeError: glitchtip webhook verification",
      "title_link": "http://glitchtip.example.com:8080/holon-test/issues/1",
      "fields": [{"title": "Project", "value": "holon-probe", "short": true}]
    }
  ]
}
```

Holon wake callbacks operate as `wake_hint`: an HTTP 200 response indicates only that the runtime accepted the wake hint; the payload is never treated as operator instruction. Once awakened, the agent queries the API directly:

- `GET /api/0/issues/{id}/`: `title` matches webhook `attachments[].title`, while `metadata.type` / `metadata.value` match the exception details.
- `GET /api/0/issues/{id}/events/`: `event_id` matches the ingestion response, and `entries` contains the full exception and message payload.

The webhook payload does not include an `event_id`. To verify a specific event, query events using the issue ID rather than assuming the webhook payload contains the full event.

## Negative Cases and Limitations

- Missing or invalid `Authorization` Bearer token: returns `401 {"detail":"Unauthorized"}`.
- Event count below `quantity`: no webhook dispatch and no wake. In our test with `quantity=5` and 1 event, no secondary webhook or callback arrived in the subsequent 60-second cycle.
- An invalid recipient endpoint (closed port) does not wake Holon. However, GlitchTip 7's test endpoint `POST .../alerts/{id}/test/` still returns `status: sent`, as connection failures are caught internally in `_post` without error logging. Do not rely on test endpoint status or instance logs to confirm delivery; verify against Holon's HTTP access logs (status 200) and packet capture.
- If a recipient URL targets a blocked address during creation, the API rejects the request immediately. Forgetting `GLITCHTIP_ALLOW_PRIVATE_IPS` for internal callbacks causes an error at creation time rather than silent failures later.

## Cleanup

`docker compose down -v` removes the database, projects, tokens, and recipients. Never leave callback URLs in stopped containers with retained volumes.

## Out of Scope

- Other recipient types such as email, Discord, or Slack.
- De-duplication policies beyond window thresholds, and uptime monitor alerts.
- UI-based project creation. This verification was conducted entirely via HTTP APIs.
