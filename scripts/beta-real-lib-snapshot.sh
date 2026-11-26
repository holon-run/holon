#!/usr/bin/env bash
# Snapshot a runtime SQLite database for the beta real-library smoke.
#
# `sqlite3 .backup` cannot snapshot a large database that a live daemon keeps
# writing: sqlite3_backup_step() restarts from page 1 whenever another
# connection modifies the source, so the copy never catches up (observed on
# the 80.9 GB live home: 3.3 GB copied after 4 minutes, then killed by the
# job timeout). `VACUUM INTO` reads the source under one snapshot and writes
# a fresh database; concurrent WAL writers do not restart it, so the snapshot
# always makes forward progress and completes.
#
# Modes:
#   --src <db> --dst <db>            consistent snapshot via VACUUM INTO
#   --src <db> --dst <db> --frozen   byte-copy an already-frozen database,
#                                    for callers that pre-made an offline copy
#
# --frozen copies the main file plus pending -wal/-journal sidecars and
# requires that no process is writing the source; -shm is volatile shared
# memory and is never copied. The destination is always a new, writable
# file, and an existing destination is refused (VACUUM INTO requires that
# too). The caller's database is never opened in place.
set -euo pipefail

SRC=""
DST=""
FROZEN=0

log() { printf '[beta-snapshot] %s\n' "$*"; }
die() { printf '[beta-snapshot] ERROR: %s\n' "$*" >&2; exit 1; }
usage() { sed -n '2,/^set -euo pipefail$/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --src) SRC="$2"; shift ;;
    --dst) DST="$2"; shift ;;
    --frozen) FROZEN=1 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
  shift
done
[ -n "$SRC" ] || die "--src is required"
[ -n "$DST" ] || die "--dst is required"
[ -f "$SRC" ] || die "source database not found: $SRC"
[ ! -e "$DST" ] || die "destination already exists: $DST"

# Quote a filesystem path as a SQLite string literal.
sql_quote() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/''/g")"; }

mkdir -p "$(dirname "$DST")"

if [ "$FROZEN" -eq 1 ]; then
  log "copying frozen database $SRC"
  cp -- "$SRC" "$DST"
  # A sidecar holds commits not yet checkpointed into the main file. Copy it
  # so the sandbox database recovers to the same state instead of silently
  # losing the tail of the WAL.
  for suffix in -wal -journal; do
    if [ -f "$SRC$suffix" ]; then
      cp -- "$SRC$suffix" "$DST$suffix"
    fi
  done
else
  command -v sqlite3 >/dev/null 2>&1 || die "sqlite3 is required"
  # VACUUM INTO needs SQLite >= 3.27.0. Probe the actual behavior instead of
  # parsing versions, so an old build fails with a precise message rather
  # than silently falling back to a .backup that never finishes.
  probe_dir="$(mktemp -d "${TMPDIR:-/tmp}/holon-beta-snapshot.XXXXXX")"
  if ! sqlite3 :memory: "VACUUM INTO $(sql_quote "$probe_dir/probe.sqlite")" >/dev/null 2>&1; then
    rm -rf "$probe_dir"
    die "sqlite3 $(sqlite3 --version 2>/dev/null | awk '{print $1}') does not support VACUUM INTO (need >= 3.27.0)"
  fi
  rm -rf "$probe_dir"
  log "VACUUM INTO snapshot of $SRC (one read transaction; concurrent writes do not restart it)"
  # Wait out a transient writer/checkpoint lock instead of failing the gate;
  # the snapshot itself still holds only a read transaction on the source.
  sqlite3 -cmd ".timeout 60000" "$SRC" "VACUUM INTO $(sql_quote "$DST")"
fi

[ -f "$DST" ] || die "snapshot did not produce $DST"
for suffix in "" -wal -journal; do
  if [ -f "$DST$suffix" ]; then chmod 600 "$DST$suffix"; fi
done
log "snapshot complete: $(du -h "$DST" | awk '{print $1}')"
