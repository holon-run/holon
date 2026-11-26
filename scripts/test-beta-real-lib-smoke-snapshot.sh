#!/usr/bin/env bash
# Tests for scripts/beta-real-lib-snapshot.sh.
#
# The regression this guards: `.backup` restarts from page 1 whenever a live
# writer modifies the source, so a large live database never finishes
# snapshotting. Here a background writer keeps committing while the snapshot
# runs; the snapshot must still complete and the result must be a consistent
# database with every seed row intact.
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
snapshot="$script_dir/beta-real-lib-snapshot.sh"
work="$(mktemp -d "${TMPDIR:-/tmp}/holon-beta-snapshot-test.XXXXXX")"
writer_pid=""

cleanup() {
  if [ -n "$writer_pid" ]; then
    kill "$writer_pid" 2>/dev/null || true
    wait "$writer_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

command -v sqlite3 >/dev/null 2>&1 || { echo "sqlite3 is required for this test" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "python3 is required for this test" >&2; exit 1; }

src="$work/src.sqlite"
snap="$work/snapshot.sqlite"
frozen="$work/frozen.sqlite"
rows=2048
blob_bytes=32768

sqlite3 "$src" 'PRAGMA journal_mode=WAL; CREATE TABLE payload(id INTEGER PRIMARY KEY, data BLOB NOT NULL); CREATE TABLE progress(n INTEGER NOT NULL);'
sqlite3 "$src" 'INSERT INTO progress(n) VALUES(0);'

python3 - "$src" "$rows" "$blob_bytes" <<'PY'
import sqlite3, sys
db, rows, size = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
conn = sqlite3.connect(db)
blob = b'x' * size
conn.executemany('INSERT INTO payload(id, data) VALUES(?, ?)', ((i, blob) for i in range(1, rows + 1)))
conn.commit()
PY

cat >"$work/writer.py" <<'PY'
import os, sqlite3, sys
db = sys.argv[1]
stop = sys.argv[2]
conn = sqlite3.connect(db, timeout=30, isolation_level=None)
blob = b'y' * 32768
i = 0
while not os.path.exists(stop):
    i += 1
    conn.execute('BEGIN IMMEDIATE')
    conn.execute('UPDATE payload SET data = ? WHERE id = ?', (blob, (i % 2048) + 1))
    conn.execute('UPDATE progress SET n = n + 1')
    conn.execute('COMMIT')
PY

progress() { sqlite3 "$src" 'SELECT n FROM progress;'; }

python3 "$work/writer.py" "$src" "$work/stop" &
writer_pid=$!

# Wait until the writer has committed at least once, so the snapshot really
# runs against a concurrently written database.
for _ in $(seq 1 100); do
  [ "$(progress)" -ge 1 ] && break
  sleep 0.1
done
before="$(progress)"
[ "$before" -ge 1 ] || { echo "background writer did not start" >&2; exit 1; }

timeout 120 "$snapshot" --src "$src" --dst "$snap"

[ -f "$snap" ] || { echo "snapshot file missing" >&2; exit 1; }
[ -w "$snap" ] || { echo "snapshot is not writable" >&2; exit 1; }
[ "$(sqlite3 "$snap" 'PRAGMA integrity_check;')" = "ok" ] || { echo "snapshot integrity_check failed" >&2; exit 1; }
[ "$(sqlite3 "$snap" 'SELECT count(*) FROM payload;')" = "$rows" ] || { echo "snapshot lost rows" >&2; exit 1; }
[ "$(sqlite3 "$snap" "SELECT count(*) FROM payload WHERE length(data) != $blob_bytes;")" = "0" ] \
  || { echo "snapshot contains a torn payload" >&2; exit 1; }

after="$(progress)"
[ "$after" -ge "$before" ] || { echo "writer progress went backwards" >&2; exit 1; }
# The writer is still running: it must keep committing after the snapshot.
for _ in $(seq 1 50); do
  [ "$(progress)" -gt "$after" ] && break
  sleep 0.1
done
[ "$(progress)" -gt "$after" ] || { echo "writer stopped making progress" >&2; exit 1; }

# Stop the writer and snapshot the now-frozen database through the copy path.
kill "$writer_pid" 2>/dev/null || true
wait "$writer_pid" 2>/dev/null || true
writer_pid=""
timeout 120 "$snapshot" --src "$src" --dst "$frozen" --frozen
[ "$(sqlite3 "$frozen" 'PRAGMA integrity_check;')" = "ok" ] || { echo "frozen copy integrity_check failed" >&2; exit 1; }
[ "$(sqlite3 "$frozen" 'SELECT count(*) FROM payload;')" = "$rows" ] || { echo "frozen copy lost rows" >&2; exit 1; }
# The copy must be a writable independent file, never the caller's database
# opened in place.
[ ! "$src" -ef "$frozen" ] || { echo "frozen copy aliases the source" >&2; exit 1; }
sqlite3 "$frozen" 'CREATE TABLE probe(x);'
[ "$(sqlite3 "$src" "SELECT count(*) FROM sqlite_master WHERE name = 'probe';")" = "0" ] \
  || { echo "writing the copy changed the source" >&2; exit 1; }

# The helper must refuse to overwrite an existing destination.
if "$snapshot" --src "$src" --dst "$snap" >/dev/null 2>&1; then
  echo "helper overwrote an existing destination" >&2
  exit 1
fi

echo "beta real-library snapshot tests passed."
