#!/usr/bin/env bash
# Real-library startup smoke for a daily beta build.
#
# Snapshots the live daemon home into a sandbox (WAL-consistent `sqlite3
# .backup` of the runtime database plus the small config/credential files),
# boots the published beta binary against the snapshot on a spare port, and
# verifies the daemon starts, passes schema migrations, and accepts
# connections. This is the gate that catches #4429-class upgrade aborts
# before any real machine auto-upgrades.
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
rm -rf "$SANDBOX"
mkdir -p "$SANDBOX/.holon/state"
chmod 700 "$SANDBOX"
cp "$HOME_SRC/config.json" "$HOME_SRC/credentials.json" "$SANDBOX/.holon/"
if [ -f "$HOME_SRC/model-discovery-cache.json" ]; then cp "$HOME_SRC/model-discovery-cache.json" "$SANDBOX/.holon/"; fi
: > "$SANDBOX/.holon/holon.db"
log "sqlite .backup of state/runtime.sqlite (WAL-consistent; can take minutes on large homes)"
sqlite3 "$HOME_SRC/state/runtime.sqlite" ".backup '$SANDBOX/.holon/state/runtime.sqlite'"
log "snapshot size: $(du -sh "$SANDBOX/.holon/state/runtime.sqlite" | awk '{print $1}')"

LOG="$SANDBOX/serve.log"
WEB_ARGS=()
[ -n "${SMOKE_WEB_DIST:-}" ] && WEB_ARGS=(--web-dist "$SMOKE_WEB_DIST")
setsid env -u HOLON_SOCKET_PATH "HOLON_HOME=$SANDBOX/.holon" \
  "$BINARY" serve --access local --port "$PORT" "${WEB_ARGS[@]}" >"$LOG" 2>&1 &
SERVER_PID=$!

# The runtime loads config, credentials, state and its socket under HOLON_HOME
# (default ~/.holon). Without an explicit override the beta would open and
# migrate the real daemon home; assert the process env is actually bound to
# the snapshot before treating the run as a real-library smoke. /proc environ
# can still be empty right after fork+exec, so retry briefly before failing.
env_bound=0
for _ in $(seq 1 10); do
  if tr '\0' '\n' <"/proc/$SERVER_PID/environ" 2>/dev/null | grep -qx "HOLON_HOME=$SANDBOX/.holon"; then
    env_bound=1
    break
  fi
  sleep 0.5
done
if [ "$env_bound" -ne 1 ]; then
  KEEP=1
  die "serve process is not bound to the sandbox HOLON_HOME; aborting to protect the real home"
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
