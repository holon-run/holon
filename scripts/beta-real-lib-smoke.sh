#!/usr/bin/env bash
# Real-library startup smoke for a daily beta build.
#
# Snapshots the live daemon home's runtime database into a sandbox
# (WAL-consistent `sqlite3 .backup` of `state/runtime.sqlite`), boots the
# published beta binary against the snapshot on a spare port, and verifies
# the daemon starts, passes schema migrations, and accepts connections. This
# is the gate that catches #4429-class upgrade aborts before any real machine
# auto-upgrades.
#
# Isolation invariants (enforced below, fail closed):
# - Zero credentials: nothing from the live home's credential surface is
#   copied into the sandbox — no `credentials.json`, no `config.json`, no
#   model-discovery cache. serve resolves a default model at startup and
#   aborts when none is authenticated, so the sandbox boots in bootstrap
#   mode (HOLON_BOOTSTRAP=1, crates/holon-runtime/src/config/mod.rs), the
#   runtime's designed no-model-yet state: the provider rejects every agent
#   turn. The serve environment is also scrubbed (`env -i`) so ambient
#   provider credential variables (for example OPENAI_API_KEY) cannot reach
#   the daemon.
# - Agents cannot execute: the snapshot carries the real runtime state
#   (timers, queued work items, waiting agents), but with no usable
#   credential no agent can make a model call, so nothing in the sandbox can
#   drive real LLM calls or external actions during the settle window.
#   Data-layer isolation alone is not execution isolation.
# - Migration smoke semantics unchanged: the sandbox database keeps the real
#   database shape, so schema migration behavior is verified exactly as
#   before.
#
# Usage: beta-real-lib-smoke.sh <beta-tag> [--binary <path>] [--keep] [--no-marker]
# Env:
#   SMOKE_HOME_SRC      default ~/.holon (live daemon home to snapshot)
#   SMOKE_SANDBOX       default /var/tmp/holon-beta-smoke (reused, replaced)
#   SMOKE_PORT          default 18099
#   SMOKE_SETTLE_SECS   seconds the beta must stay healthy (default 45)
#   SMOKE_WEB_DIST      optional --web-dist path if the build needs one
# On success the `[smoke: pending]` marker in the beta release body is
# replaced with `[smoke: passed] ...` (requires `gh`; skip with --no-marker).
set -euo pipefail

REPO="holon-run/holon"
TAG=""
BINARY=""
KEEP=0
MARKER=1
HOME_SRC="${SMOKE_HOME_SRC:-$HOME/.holon}"
SANDBOX="${SMOKE_SANDBOX:-/var/tmp/holon-beta-smoke}"
PORT="${SMOKE_PORT:-18099}"
SETTLE="${SMOKE_SETTLE_SECS:-45}"
WORK=""

log() { printf '[beta-smoke] %s\n' "$*"; }
die() { printf '[beta-smoke] ERROR: %s\n' "$*" >&2; exit 1; }
cleanup() {
  if [ -n "${SERVER_PID:-}" ]; then kill -TERM -- "-${SERVER_PID}" 2>/dev/null || true; fi
  if [ "$KEEP" -ne 1 ] && [ -n "$WORK" ]; then rm -rf "$WORK"; fi
}
trap cleanup EXIT

while [ $# -gt 0 ]; do
  case "$1" in
    --binary) BINARY="$2"; shift ;;
    --keep) KEEP=1 ;;
    --no-marker) MARKER=0 ;;
    -h|--help) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) TAG="$1" ;;
  esac
  shift
done
[ -n "$TAG" ] || die "usage: beta-real-lib-smoke.sh <beta-tag> [--binary <path>] [--keep] [--no-marker]"
case "$TAG" in *-beta.*) ;; *) die "$TAG does not look like a beta tag" ;; esac

OS="$(uname -s)"
ARCH="$(uname -m)"
case "${OS}/${ARCH}" in
  Linux/x86_64) ASSET=holon-linux-amd64 ;;
  *) die "this smoke runs on the real-library Linux host (got ${OS}/${ARCH})" ;;
esac

WORK="$(mktemp -d "${TMPDIR:-/tmp}/holon-beta-smoke-download.XXXXXX")"
if [ -z "$BINARY" ]; then
  log "downloading ${TAG} ${ASSET} asset"
  curl -fsSL "https://github.com/${REPO}/releases/download/${TAG}/${ASSET}.tar.gz" -o "$WORK/${ASSET}.tar.gz"
  curl -fsSL "https://github.com/${REPO}/releases/download/${TAG}/checksums.txt" -o "$WORK/checksums.txt"
  (
    cd "$WORK"
    grep -F "${ASSET}.tar.gz" checksums.txt | sed 's#\./##g' > want.txt
    [ -s want.txt ] || { echo "asset missing from checksums.txt" >&2; exit 1; }
    if command -v sha256sum >/dev/null 2>&1; then sha256sum -c want.txt; else shasum -a 256 -c want.txt; fi
  )
  tar -xzf "$WORK/${ASSET}.tar.gz" -C "$WORK"
  BINARY="$(find "$WORK" -type f -name holon -perm -u+x | head -n 1)"
  [ -n "$BINARY" ] || die "holon binary not found inside the asset"
fi
log "smoking beta binary: $("$BINARY" --version)"

log "snapshotting live daemon home ${HOME_SRC} into sandbox ${SANDBOX}"
if [ -e "$HOME_SRC/credentials.json" ]; then
  log "live credentials.json detected; NOT copying it (sandbox must stay credential-free)"
fi
rm -rf "$SANDBOX"
mkdir -p "$SANDBOX/.holon/state"
chmod 700 "$SANDBOX"
: > "$SANDBOX/.holon/holon.db"
log "sqlite .backup of state/runtime.sqlite (WAL-consistent; can take minutes on large homes)"
sqlite3 "$HOME_SRC/state/runtime.sqlite" ".backup '$SANDBOX/.holon/state/runtime.sqlite'"
log "snapshot size: $(du -sh "$SANDBOX/.holon/state/runtime.sqlite" | awk '{print $1}')"

# Fail closed: the sandbox must be credential-free and config-free before
# boot. The snapshot step never copies these files; this guards the invariant
# against future edits or stray files turning the sandbox into an executable
# agent home. The sandbox is also the daemon's HOME, so a stray .codex home
# (codex CLI OAuth) would make the built-in openai-codex provider ready.
if [ -e "$SANDBOX/.holon/credentials.json" ] \
  || [ -e "$SANDBOX/.holon/config.json" ] \
  || [ -e "$SANDBOX/.codex/auth.json" ]; then
  rm -rf "$SANDBOX"
  unset SANDBOX
  die "sandbox would contain credentials.json/config.json/.codex/auth.json; refusing to boot an executable agent home"
fi

LOG="$SANDBOX/serve.log"
WEB_ARGS=()
[ -n "${SMOKE_WEB_DIST:-}" ] && WEB_ARGS=(--web-dist "$SMOKE_WEB_DIST")
# A credential-free sandbox has no authenticated provider, so serve would
# abort at startup ("no default model configured ..."). HOLON_BOOTSTRAP=1
# selects bootstrap mode instead: the daemon boots and every agent turn is
# rejected by the bootstrap provider, so execution isolation is unchanged.
# HOME/CODEX_HOME are pinned into the sandbox because the built-in
# openai-codex provider falls back to the relative path ./.codex when HOME is
# unset (holon-config default_codex_home). Leaving HOME unset would let a
# .codex/auth.json next to the invocation directory authenticate a real agent
# turn and silently defeat HOLON_BOOTSTRAP=1.
setsid env -i PATH="$PATH" "HOME=$SANDBOX" "CODEX_HOME=$SANDBOX/.codex" \
  "HOLON_HOME=$SANDBOX/.holon" "HOLON_BOOTSTRAP=1" \
  "$BINARY" serve --access local --port "$PORT" "${WEB_ARGS[@]}" >"$LOG" 2>&1 &
SERVER_PID=$!

# The serve environment is scrubbed (env -i): no ambient credential variables
# can reach the sandbox daemon, which resolves built-in provider auth from
# environment variables when they are present.

# The runtime loads config, credentials, state and its socket under HOLON_HOME
# (default ~/.holon). Without an explicit override the beta would open and
# migrate the real daemon home; assert the process env is actually bound to
# the snapshot before treating the run as a real-library smoke. HOME and
# CODEX_HOME are asserted too: a dropped binding would re-open the host
# credential lookup paths. /proc environ can still be empty right after
# fork+exec, so retry briefly before failing.
env_bound=0
for _ in $(seq 1 10); do
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "---- serve log tail ----" >&2
    tail -n 20 "$LOG" >&2 || true
    die "serve process exited during the env check (see log above)"
  fi
  proc_env="$(tr '\0' '\n' <"/proc/$SERVER_PID/environ" 2>/dev/null || true)"
  if printf '%s\n' "$proc_env" | grep -qxF "HOLON_HOME=$SANDBOX/.holon" \
    && printf '%s\n' "$proc_env" | grep -qxF "HOME=$SANDBOX" \
    && printf '%s\n' "$proc_env" | grep -qxF "CODEX_HOME=$SANDBOX/.codex"; then
    env_bound=1
    break
  fi
  sleep 0.5
done
if [ "$env_bound" -ne 1 ]; then
  KEEP=1
  die "serve process is not bound to the sandbox HOLON_HOME/HOME/CODEX_HOME; aborting to protect the real home"
fi

healthy=0
for _ in $(seq 1 120); do
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    break
  fi
  if (exec 3<>"/dev/tcp/127.0.0.1/${PORT}") 2>/dev/null; then
    exec 3>&- 3<&- || true
    healthy=1
    break
  fi
  sleep 1
done
if [ "$healthy" -ne 1 ]; then
  echo "---- serve log tail ----" >&2
  tail -n 80 "$LOG" >&2 || true
  echo "---- end ----" >&2
  KEEP=1
  die "beta did not open port ${PORT} within 120s (sandbox kept at ${SANDBOX})"
fi
log "daemon is listening on ${PORT}; settling ${SETTLE}s to catch delayed aborts"
sleep "$SETTLE"
if ! kill -0 "$SERVER_PID" 2>/dev/null; then
  KEEP=1
  tail -n 80 "$LOG" >&2 || true
  die "beta daemon exited during the settle window (sandbox kept at ${SANDBOX})"
fi
if grep -qiE 'migration[^a-z]*(abort|fail)|aborting|migrat[^a-z]*error|panicked|fatal error|failed to start' "$LOG"; then
  KEEP=1
  tail -n 80 "$LOG" >&2 || true
  die "beta log matches a startup-abort signature (sandbox kept at ${SANDBOX})"
fi
log "PASS: beta ${TAG} started against the real-library snapshot and stayed healthy ${SETTLE}s"

if [ "$MARKER" -eq 1 ]; then
  command -v gh >/dev/null 2>&1 || { log "gh not available; skipping release-body marker"; exit 0; }
  body="$(gh release view "$TAG" --repo "$REPO" --json body --jq .body)"
  case "$body" in
    *"[smoke: pending]"*)
      newbody="$(printf '%s' "$body" | sed "s#\[smoke: pending\]#[smoke: passed] $(date -u +%FT%TZ) real-library snapshot smoke (linux-amd64)#")"
      gh release edit "$TAG" --repo "$REPO" --notes "$newbody" >/dev/null
      log "release-body marker flipped to [smoke: passed]"
      ;;
    *"[smoke: passed]"*) log "marker already set; nothing to do" ;;
    *) log "release body has no smoke marker; not modifying" ;;
  esac
fi
