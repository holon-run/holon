# Daily beta train

Every day a beta build of `main` is cut, trialed in a real environment for 24
hours, and — if it stays healthy — promoted to a stable release. The whole
loop is automatic; there is no human confirmation gate.

## Channel contract

Publishing to a channel is itself the upgrade signal for machines on that
channel:

| Channel | What it is | Who installs it |
|---|---|---|
| beta | A `vX.Y.Z-beta.<date>` GitHub **prerelease**. It is only tagged after the Release E2E attestation for the candidate SHA is green, and the demo canary additionally requires the `[smoke: passed]` marker in the release body (written by the real-library smoke). | `demo.holon.run` (Railway), upgraded by the release orchestrator |
| stable | A normal GitHub release, cut only after the previous day's beta passed the 24h demo trial, with the standard dual verification (Release E2E attestation + main CI) and full channel verification. | The operator's own machines, one stage behind the canary |

The rollout order (canary first, owner machines after promotion) is enforced by
the channels, not by machine-to-machine communication. A workstation that may be
powered off (the Mac) is deliberately not part of the gate: the canary lives in
the always-on Railway demo service.

## Daily timeline (Beijing = UTC+8)

The train is **not** on a GitHub cron. The release orchestrator
(`holon-release`) owns a daily timer that fires after the scheduled E2E and
dispatches the train, so every triggered run is downstream of a green nightly
E2E.

| Time | Actor | Action |
|---|---|---|
| 02:00 Beijing (18:00 UTC) | E2E Scheduler Nightly (`0 18 * * *`) | Scheduled E2E runs (~20-30 min). GitHub can delay scheduled runs by hours; the train tolerates that via the 36h freshness gate below. The nightly full-sweep CI (`40 2 * * *`, 10:40 Beijing) stays on UTC and is not a train gate. |
| 02:40 Beijing (18:40 UTC) | `holon-release` timer | 24h promotion gate for yesterday's beta (below). Pass → standard prep PR → dual verification → stable tag → Release workflow → channel verification. |
| 03:00 Beijing (19:00 UTC) | `holon-release` timer | Dispatch `beta-train.yml` (`workflow_dispatch`). Preflight refuses to run unless the latest scheduled E2E run is green and less than 36h old; then requires no open `priority:critical` (P0) issues, new commits on `main` in the last 24h, the tag free, main CI green on HEAD, and no queued/running Release workflow (milestone releases keep the stable slot). Dispatches the Release E2E attestation (`candidate_ref` = main HEAD, `release_tag` = the beta tag, `previous_ref` = latest stable). |
| ~03:10–06:00 Beijing (19:10Z–22:00Z) | `beta-train.yml` | When the attestation is green and its artifact is bound to this candidate SHA + tag, tags `vX.Y.Z-beta.<date>` on main HEAD, then dispatches the Release workflow on that tag (a `GITHUB_TOKEN` tag push does not trigger the tag-push workflow). Release runs in **beta mode**: CLI binaries + GHCR `:beta` + `:<version>` image, GitHub prerelease, no latest/Homebrew/appcast/mobile apps. An interrupted train resumes: an existing tag without a prerelease is re-dispatched instead of skipped, and Release updates the existing prerelease in place. |
| after publish | `scripts/beta-real-lib-smoke.sh` (runs on the real-library Linux host) | Snapshots the live daemon home's runtime database (WAL-consistent sqlite `.backup` of `state/runtime.sqlite`) into a zero-credential sandbox — no `config.json`, `credentials.json`, or model-discovery cache is copied, and the serve environment is scrubbed (`env -i`), so no agent in the sandbox can make a model call or take any external action while the real database shape is still what gets migrated — then boots the published beta against it and flips the release-body marker to `[smoke: passed]`. |
| after publish | `scripts/demo-trial.sh run --image ghcr.io/holon-run/holon:<version-tag>` | Requires the candidate beta version (explicit `--expect-version` or a version-bearing image tag), points the Railway demo service at the beta image, waits for **this** deployment (bound to the new deployment id, not the mutable source image) to succeed, verifies the running daemon reports that `product_version`, and a live agent round-trip. The 24h trial starts here. |
| next day 02:40 Beijing | `holon-release` timer | Promotion gate: attestation green, the demo ran the beta for ≥20h without a failed deployment or restart loop, the agent round-trip is healthy at the end of the window, no release-blocking issue, stable slot free. Pass → stable promotion. Fail → the beta dies; the next train restarts from the new `main`. |
| after stable verification | owner machine(s) | `channel-upgrade.sh` installs the new stable and restarts. |

## Operator release principles

Two operator-set gates (2026-11-24) apply to every daily beta:

1. **An open P0 blocks the train.** While any issue labeled `priority:critical`
   (the repository's P0 equivalent) is open, the preflight skips the day and the
   train waits until the issue is resolved.
2. **A red main CI is work, not a skip.** When main CI fails on the HEAD commit,
   the preflight skips the day and `holon-release` diagnoses the failure: a real
   regression is fixed (dispatching a developer agent for release-blocking
   fixes) before the next train is dispatched; only a proven infrastructure
   flake unblocks the train without a code fix.

**Docs-only HEAD inherits the last green main CI.** `ci.yml` triggers on a
positive paths whitelist, so a docs-only commit on `main` produces no CI run at
all. The preflight therefore classifies four main-CI states separately
(`success`, `pending`, `blocked`, `missing`) and only lets `missing` through
when `scripts/beta_train_ci_gate.sh` proves that every file changed since the
last green main CI is outside `ci.yml`'s push paths. Inheritance replaces only
that one evidence point; attestation, tagging, and the Release handoff are
unchanged.

## Demo canary on Railway

`demo.holon.run` is the always-on trial environment: a Railway service built
from this repository's `Dockerfile` with one persistent volume, so the beta runs
against the same real library that a user would upgrade. Requirements:

- `RAILWAY_TOKEN` for the workspace that owns the demo service (discovery is by
  the `demo.holon.run` custom domain, so no project IDs are hardcoded).
- `DEMO_CONTROL_TOKEN` and `DEMO_AGENT_ID` for the round-trip probe: the
  verifier prompts that agent and waits for a brief containing the probe.

```bash
scripts/demo-trial.sh discover            # project/environment/service for demo.holon.run
scripts/demo-trial.sh upgrade --image ghcr.io/holon-run/holon:v0.62.0-beta.20261124
scripts/demo-trial.sh verify --expect-version 0.62.0-beta.20261124
scripts/demo-trial.sh run --image ghcr.io/holon-run/holon:v0.62.0-beta.20261124 \
  --expect-version 0.62.0-beta.20261124
```

`verify` fails loudly on an unhealthy control status, a different
`product_version`, or no agent reply inside the probe budget. A failed demo
upgrade blocks promotion and leaves the stable channel untouched.

## Machine setup

Owner machines run `scripts/channel-upgrade.sh` from a scheduler. Install it
anywhere (for example `/usr/local/bin` or `~/bin`); it needs `bash`, `curl`,
`jq`, and (optionally, for heartbeat reporting) an authenticated `gh`.

### Linux (stable channel): systemd user units

`~/.config/systemd/user/holon-upgrade.service`:

```ini
[Unit]
Description=Holon stable channel upgrader

[Service]
Type=oneshot
Environment=HOLON_CHANNEL=stable
Environment=HOLON_HEARTBEAT_ISSUE=<heartbeat issue number>
ExecStart=%h/bin/channel-upgrade.sh
```

`~/.config/systemd/user/holon-upgrade.timer`:

```ini
[Unit]
Description=Check the Holon stable channel every 2 hours

[Timer]
OnCalendar=*-*-* 09/2:30:00
Persistent=true

[Install]
WantedBy=timers.target
```

Restart handling defaults to `systemctl --user restart holon.service`; the
daemon must be a systemd user unit for the default health check to work.

### macOS (optional beta channel): launchd

Not part of the release gate: the Mac may be powered off, so it must never be
the canary. Use it only as an extra beta-channel observer.

`~/Library/LaunchAgents/run.holon.channel-upgrade.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>run.holon.channel-upgrade</string>
  <key>ProgramArguments</key>
  <array><string>/bin/bash</string><string>%/path/to/channel-upgrade.sh</string></array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HOLON_CHANNEL</key><string>beta</string>
    <key>HOLON_RESTART_CMD</key><string>&lt;command that restarts the Mac daemon&gt;</string>
    <key>HOLON_HEARTBEAT_ISSUE</key><string>&lt;heartbeat issue number&gt;</string>
    <key>PATH</key><string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
  </dict>
  <key>StartCalendarInterval</key>
  <array>
    <dict><key>Hour</key><integer>13</integer><key>Minute</key><integer>30</integer></dict>
    <dict><key>Hour</key><integer>15</integer><key>Minute</key><integer>30</integer></dict>
    <dict><key>Hour</key><integer>17</integer><key>Minute</key><integer>30</integer></dict>
  </array>
  <key>StandardOutPath</key><string>/tmp/holon-channel-upgrade.log</string>
  <key>StandardErrorPath</key><string>/tmp/holon-channel-upgrade.log</string>
</dict></plist>
```

macOS has no safe default daemon restart: set `HOLON_RESTART_CMD` to the
`launchctl kickstart -k gui/$(id -u)/<service-label>` line (or whatever
restarts the Mac daemon). The previous binary is always kept at
`$(dirname holon)/holon.prev` for manual rollback.

## Failure handling

- Attestation red or LLM quota exhausted → no beta is tagged that day; the
  train reports to the heartbeat issue and the stable channel is untouched.
- Real-library smoke failure → the marker stays `[smoke: pending]`, beta
  machines never upgrade, and the failure needs a fix before any promotion.
- Demo deployment failed, or the demo is unhealthy / cannot complete the agent
  round-trip at the gate → no stable promotion; owner machines stay on the last
  verified stable.
- Beta binaries may contain forward schema migrations: a canary that took a
  migration cannot simply downgrade. Rollback for beta machines means
  restoring the pre-trial backup, not swapping the binary back.
