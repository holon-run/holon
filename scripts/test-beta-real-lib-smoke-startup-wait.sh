#!/usr/bin/env bash
# Tests for the serve port-wait behavior of scripts/beta-real-lib-smoke.sh.
#
# Regression: a large real home (61 GB observed) migrates for minutes before
# serve binds its port, so the old fixed `seq 1 120` wall-clock cap failed a
# healthy migration with "did not open port ... within 120s". The wait is now
# an inactivity window (SMOKE_STARTUP_WAIT_SECS) that migration log progress
# refreshes, and timeout messages distinguish a dead/aborting daemon from one
# that is still migrating.
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
smoke="$script_dir/beta-real-lib-smoke.sh"
work="$(mktemp -d "${TMPDIR:-/tmp}/holon-beta-startup.XXXXXX")"

cleanup() { rm -rf "$work"; }
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

command -v sqlite3 >/dev/null 2>&1 || { echo "sqlite3 is required for this test" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "python3 is required for this test" >&2; exit 1; }

db="$work/db.sqlite"
sqlite3 "$db" 'CREATE TABLE payload(id INTEGER PRIMARY KEY, data TEXT);'

free_port() {
  python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

cat >"$work/listen.py" <<'PY'
import socket, sys, time
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1])))
s.listen(16)
time.sleep(120)
PY

# A stand-in beta binary. The smoke only needs `--version` and a `serve`
# invocation whose real work is replaced by one of the startup behaviors
# below; it never touches the snapshot database.
write_fake_binary() {
  local path="$1" behavior="$2"
  cat >"$path" <<EOF
#!/usr/bin/env bash
set -euo pipefail
if [ "\${1:-}" = "--version" ]; then echo "holon fake 0.0.0"; exit 0; fi
port=""
while [ \$# -gt 0 ]; do
  case "\$1" in
    --port) port="\$2"; shift ;;
  esac
  shift
done
[ -n "\$port" ] || { echo "fake beta: missing --port" >&2; exit 2; }
progress() {
  echo "INFO starting runtime database migration version=\$1 name=fake"
  echo "INFO finished runtime database migration version=\$1 name=fake"
}
case "${behavior}" in
  progressing)
    # Advancing migrations for longer than the window must not be a failure.
    for i in 1 2 3 4 5 6; do progress "\$i"; sleep 1; done
    exec python3 "${work}/listen.py" "\$port"
    ;;
  stalled)
    progress 1
    sleep 120
    ;;
  exiting)
    progress 1
    sleep 1
    exit 3
    ;;
  aborting)
    echo "ERROR runtime database migration failed version=109 name=fake"
    sleep 120
    ;;
  silent-busy)
    # No log output at all: only process CPU time proves it is still working.
    end=\$((SECONDS + 4))
    while [ "\$SECONDS" -lt "\$end" ]; do :; done
    exec python3 "${work}/listen.py" "\$port"
    ;;
  *) echo "fake beta: unknown behavior ${behavior}" >&2; exit 2 ;;
esac
EOF
  chmod +x "$path"
}

run_smoke() { # $1=fake binary, $2=sandbox, $3=startup wait, $4=output file
  SMOKE_SANDBOX="$2" SMOKE_SETTLE_SECS=0 SMOKE_STARTUP_WAIT_SECS="$3" \
    SMOKE_PORT="$(free_port)" \
    bash "$smoke" "v0.0.0-beta.20260101" --binary "$1" --db "$db" --no-marker \
    >"$4" 2>&1
}

# 1. A migration that keeps logging progress past the window still passes.
write_fake_binary "$work/progressing" progressing
if ! run_smoke "$work/progressing" "$work/sandbox-progressing" 2 "$work/out-progressing"; then
  cat "$work/out-progressing" >&2
  fail "healthy progressing migration was reported as a failure"
fi
grep -q "extending the port wait" "$work/out-progressing" \
  || fail "progressing run did not refresh the port wait"
grep -q "PASS:" "$work/out-progressing" || fail "progressing run did not pass"

# 2. A live daemon that stops making log progress fails after the window, and
#    the message says the process is still running and echoes the window.
write_fake_binary "$work/stalled" stalled
if run_smoke "$work/stalled" "$work/sandbox-stalled" 2 "$work/out-stalled"; then
  fail "stalled startup unexpectedly passed"
fi
grep -q "within 2s of progress" "$work/out-stalled" \
  || fail "stalled timeout did not echo the configured window"
grep -q "process still running" "$work/out-stalled" \
  || fail "stalled timeout did not distinguish a live process"
grep -q "SMOKE_STARTUP_WAIT_SECS" "$work/out-stalled" \
  || fail "stalled timeout did not point at SMOKE_STARTUP_WAIT_SECS"

# 3. A daemon that exits while migrating gets the process-exit message, not
#    the generic port timeout.
write_fake_binary "$work/exiting" exiting
if run_smoke "$work/exiting" "$work/sandbox-exiting" 2 "$work/out-exiting"; then
  fail "exiting startup unexpectedly passed"
fi
grep -q "serve process exited before opening port" "$work/out-exiting" \
  || fail "exited startup was not reported as a process exit"

# 4. A log that matches an abort signature fails fast with its own message
#    instead of waiting out the window.
write_fake_binary "$work/aborting" aborting
start="$(date +%s)"
if run_smoke "$work/aborting" "$work/sandbox-aborting" 30 "$work/out-aborting"; then
  fail "aborting startup unexpectedly passed"
fi
[ $(( $(date +%s) - start )) -lt 20 ] || fail "abort signature did not fail fast"
grep -q "startup-abort signature" "$work/out-aborting" \
  || fail "abort signature was not distinguished from a timeout"

# 5. CPU activity without log output is progress too: a silent large
#    migration must not time out while the process is still working.
write_fake_binary "$work/silent-busy" silent-busy
if ! run_smoke "$work/silent-busy" "$work/sandbox-silent-busy" 2 "$work/out-silent-busy"; then
  cat "$work/out-silent-busy" >&2
  fail "silent but CPU-busy startup was reported as a failure"
fi

# 6. An invalid window is rejected before any snapshot or launch work.
if SMOKE_STARTUP_WAIT_SECS=abc bash "$smoke" "v0.0.0-beta.20260101" \
  --binary "$work/progressing" --db "$db" --no-marker >"$work/out-invalid" 2>&1; then
  fail "invalid SMOKE_STARTUP_WAIT_SECS was accepted"
fi
grep -q "SMOKE_STARTUP_WAIT_SECS must be a positive integer" "$work/out-invalid" \
  || fail "invalid window was not explained"

echo "all beta smoke startup-wait tests passed"
