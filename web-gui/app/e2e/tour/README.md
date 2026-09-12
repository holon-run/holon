# Repeatable website tour

This is synthetic demo data served to the real `web-gui/app` application.
It does not connect to a Holon daemon, read credentials, access repositories,
or restore a complete Runtime. `/demo/*` paths are fictional labels.

`scenario.mjs` is the versioned source of truth: six agents, their roles,
workspace bindings, skills, work items, review plan, checklist and result briefs.
Every new fixture session is rebuilt from this module. Browser/session mutations
are disposable; this is **scenario reconstruction, not Runtime persistence**.

## Start / reset

From the repository root:

```sh
cd web-gui/app
npm ci
npx playwright install chromium
node e2e/fixture-server.mjs --port 43127 --tour
```

Open `http://127.0.0.1:43127/agents/reviewer`. Stop with Ctrl-C. Restart the same
command and use a fresh browser context to reset server and browser-local state.
The fixture binds only to loopback; unknown API routes return 404, never proxy
to real data. It has no authentication and must not be exposed publicly.
Without `--tour`, the original test fixture is unchanged.

## Verify / capture

Stop the manual server first, then from `web-gui/app`:

```sh
node e2e/tour/capture.mjs
npm run test:e2e
```

The capture command owns and stops its fixture servers and browser in `finally`.
It compares all six agent states across a server restart, starts a fresh browser,
blocks non-fixture network requests, verifies the reviewer content, and writes:

- `docs/website/assets/holon-tour-agents.webp`
- `docs/website/assets/holon-tour-review-work.webp`

Both are 1600 × 1000 WebP images. Conversion uses browser canvas; no additional
image tool is required. Set `HOLON_TOUR_PORT` to use another free loopback port.
Do not run captures concurrently on the same port.

Suggested captions:

- Six role-specific agents; the reviewer waits for operator confirmation with
  its workspace, skills and current work visible.
- The review WorkItem preserves the proposed scope, blocking decision and
  checklist independently of the conversation.

These screenshots illustrate synthetic state, not a live execution or a claim
that any review, merge or release actually occurred.
