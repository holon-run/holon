#!/usr/bin/env bash
# Channel-based Holon upgrader for unattended machines.
#
# Installs the latest published GitHub release for the configured channel and
# restarts the local daemon. Designed to run from a scheduler (systemd timer
# or launchd):
#
#   channel=stable  latest non-prerelease release. Machines on this channel
#                   only ever run builds whose beta trial completed and whose
#                   stable release passed full channel verification.
#   channel=beta    latest `vX.Y.Z-beta.<date>` prerelease whose body carries
#                   a `[smoke: passed]` marker. The Release E2E attestation
#                   gate runs before such a prerelease can be tagged at all,
#                   and the marker is written by the real-library smoke.
#
# Equal version => no-op. The candidate must also compare strictly newer than
# the installed version, so the script never downgrades.
#
# Required environment:
#   HOLON_CHANNEL   stable | beta
# Optional environment:
#   HOLON_REPO              default holon-run/holon
#   HOLON_INSTALL_DIR       default: directory of the `holon` on PATH
#                           (fallback ~/.cargo/bin)
#   HOLON_RESTART_CMD       default `systemctl --user restart holon.service`
#                           on Linux. On macOS this MUST be set (for example
#                           the `launchctl kickstart -k ...` line of your
#                           daemon service); there is no safe default.
#   HOLON_STATE_UNIT        systemd unit used for the health check
#                           (default holon.service, Linux only)
#   HOLON_HEARTBEAT_ISSUE   GitHub issue number for heartbeat comments;
#                           requires `gh` auth. Omit to skip reporting.
#   SMOKE_TIMEOUT_SECS      daemon health-check window (default 60)
# Flags: --dry-run | --force | --help
set -euo pipefail

REPO="${HOLON_REPO:-holon-run/holon}"
CHANNEL="${HOLON_CHANNEL:-}"
ISSUE="${HOLON_HEARTBEAT_ISSUE:-}"
STATE_UNIT="${HOLON_STATE_UNIT:-holon.service}"
HEALTH_TIMEOUT="${SMOKE_TIMEOUT_SECS:-60}"
DRY_RUN=0
FORCE=0
CURRENT_STEP="init"

log() { printf '[channel-upgrade] %s\n' "$*"; }
die() { printf '[channel-upgrade] ERROR: %s\n' "$*" >&2; exit 1; }

usage() {
  sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'
  exit 0
}

on_error() {
  status=$?
  heartbeat "FAILED (exit ${status}) during step '${CURRENT_STEP}'; no changes were rolled back automatically — see the upgrader log." || true
  exit "$status"
}
trap on_error ERR

heartbeat() {
  [ -n "$ISSUE" ] || return 0
  command -v gh >/dev/null 2>&1 || return 0
  gh issue comment "$ISSUE" --repo "$REPO" --body "channel-upgrade (${CHANNEL}): $*" >/dev/null 2>&1
}

latest_for_channel() {
  local jq_filter
  if [ "$CHANNEL" = "stable" ]; then
    jq_filter='[.[] | select(.draft == false and .prerelease == false)][0]'
  else
    jq_filter='[.[] | select(.draft == false and .prerelease == true and (.tag_name | contains("-beta.")) and (.body | contains("[smoke: passed]")))][0]'
  fi
  curl -fsSL "https://api.github.com/repos/${REPO}/releases?per_page=30" |
    jq -r "${jq_filter} | if . == null then empty else .tag_name end"
}

installed_version() {
  local out
  out="$("$INSTALL_DIR/holon" --version 2>/dev/null || true)"
  printf '%s' "${out#holon }" | awk '{print $1}'
}

is_newer() { # $1 candidate, $2 installed
  [ "$1" != "$2" ] || return 1
  [ "$(printf '%s\n' "$1" "$2" | sort -V | head -n 1)" = "$2" ]
}

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --force) FORCE=1 ;;
    -h|--help) usage ;;
    *) die "unknown flag: $1" ;;
  esac
  shift
done

case "$CHANNEL" in
  stable|beta) ;;
  *) die "HOLON_CHANNEL must be 'stable' or 'beta'" ;;
esac
command -v jq >/dev/null 2>&1 || die "jq is required"
command -v curl >/dev/null 2>&1 || die "curl is required"

OS="$(uname -s)"
ARCH="$(uname -m)"
case "${OS}/${ARCH}" in
  Linux/x86_64) ASSET=holon-linux-amd64 ;;
  Darwin/arm64) ASSET=holon-darwin-arm64 ;;
  Darwin/x86_64) ASSET=holon-darwin-amd64 ;;
  *) die "unsupported platform ${OS}/${ARCH}" ;;
esac

if [ -n "${HOLON_INSTALL_DIR:-}" ]; then
  INSTALL_DIR="$HOLON_INSTALL_DIR"
elif command -v holon >/dev/null 2>&1; then
  INSTALL_DIR="$(cd "$(dirname "$(command -v holon)")" && pwd)"
else
  INSTALL_DIR="$HOME/.cargo/bin"
fi
mkdir -p "$INSTALL_DIR"

CURRENT_STEP="resolve-candidate"
TAG="$(latest_for_channel)"
if [ -z "$TAG" ]; then
  log "no published release matches the ${CHANNEL} channel yet; nothing to do"
  exit 0
fi
CANDIDATE="${TAG#v}"
INSTALLED="$(installed_version)"
log "channel=${CHANNEL} candidate=${CANDIDATE} (${TAG}) installed=${INSTALLED:-none}"

if [ "$FORCE" -ne 1 ] && ! is_newer "$CANDIDATE" "${INSTALLED:-0}"; then
  log "already up to date; nothing to do"
  exit 0
fi

if [ "$DRY_RUN" -eq 1 ]; then
  log "dry-run: would install ${TAG} (${ASSET}) and restart the daemon"
  exit 0
fi

CURRENT_STEP="download"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/holon-upgrade.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
curl -fsSL "https://github.com/${REPO}/releases/download/${TAG}/${ASSET}.tar.gz" -o "$WORK/${ASSET}.tar.gz"
curl -fsSL "https://github.com/${REPO}/releases/download/${TAG}/checksums.txt" -o "$WORK/checksums.txt"

CURRENT_STEP="verify-checksum"
(
  cd "$WORK"
  # checksums.txt lines reference `./<asset>.tar.gz` (the packaging cwd);
  # normalize them to bare filenames so sha256sum -c resolves files in $WORK.
  grep -F "${ASSET}.tar.gz" checksums.txt | sed 's#\./##g' > want.txt
  [ -s want.txt ] || { echo "asset ${ASSET}.tar.gz missing from checksums.txt" >&2; exit 1; }
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c want.txt
  else
    shasum -a 256 -c want.txt
  fi
)

CURRENT_STEP="extract"
tar -xzf "$WORK/${ASSET}.tar.gz" -C "$WORK"
BINARY="$(find "$WORK" -type f -name holon -perm -u+x | head -n 1)"
[ -n "$BINARY" ] || die "could not find the holon binary inside ${ASSET}.tar.gz"
"$BINARY" --version >/dev/null

CURRENT_STEP="install"
if [ -f "$INSTALL_DIR/holon" ]; then
  cp -f "$INSTALL_DIR/holon" "$INSTALL_DIR/holon.prev"
fi
install -m 0755 "$BINARY" "$INSTALL_DIR/holon.new"
mv -f "$INSTALL_DIR/holon.new" "$INSTALL_DIR/holon"
GOT="$("$INSTALL_DIR/holon" --version | awk '{print $2}')"
[ "$GOT" = "$CANDIDATE" ] || die "installed binary reports version $GOT, expected $CANDIDATE"
log "installed ${CANDIDATE} into ${INSTALL_DIR}/holon (previous binary kept as holon.prev)"

CURRENT_STEP="restart-daemon"
if [ -n "${HOLON_RESTART_CMD:-}" ]; then
  bash -c "$HOLON_RESTART_CMD"
elif [ "$OS" = "Linux" ]; then
  systemctl --user restart "$STATE_UNIT"
else
  die "HOLON_RESTART_CMD must be set on macOS (no safe default restart)"
fi

CURRENT_STEP="health-check"
DEAD=1
for _ in $(seq 1 "$HEALTH_TIMEOUT"); do
  if [ "$OS" = "Linux" ]; then
    [ "$(systemctl --user is-active "$STATE_UNIT" 2>/dev/null || true)" = "active" ] || { DEAD=0; break; }
  fi
  pgrep -f "$INSTALL_DIR/holon serve" >/dev/null 2>&1 || { DEAD=0; break; }
  DEAD=1
  sleep 1
done
if [ "$DEAD" -eq 0 ]; then
  heartbeat "daemon is NOT healthy after upgrading ${INSTALLED:-none} -> ${CANDIDATE}. Manual attention required; the previous binary is at ${INSTALL_DIR}/holon.prev."
  die "daemon health check failed after upgrade"
fi

log "daemon healthy on ${CANDIDATE}"
heartbeat "upgraded ${INSTALLED:-none} -> ${CANDIDATE} (${TAG}, ${ASSET}); daemon healthy."
