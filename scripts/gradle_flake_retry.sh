#!/usr/bin/env bash
# Bounded Gradle retry for known rerun-cured CI flake signatures.
#
# Usage (from a Gradle project root, e.g. apps/android):
#   bash ../../scripts/gradle_flake_retry.sh ./gradlew --no-daemon <tasks...>
#
# Runs the given command with combined output tee'd to a log file and retries
# only when that log matches a known rerun-cured flake signature:
#   - #4098: AGP incremental packaging IncrementalSplitterRunnable crash.
#   - #4351: D8 dex-merge heap exhaustion (DexArchiveMergerException plus
#     java.lang.OutOfMemoryError: Java heap space) in the Gradle build JVM.
# Any other failure exits immediately so real build or test failures are not
# masked. Exhausting the attempt budget also fails the step.
#
# Environment overrides (test seams):
#   GRADLE_FLAKE_RETRY_MAX_ATTEMPTS  attempt budget, default 3
#   GRADLE_FLAKE_RETRY_BACKOFF       seconds to sleep between attempts,
#                                    default attempt*10
#   GRADLE_FLAKE_RETRY_LOG           log path, default
#                                    $RUNNER_TEMP/gradle-build.log
set -uo pipefail

if [ $# -lt 1 ]; then
  echo "usage: $0 <gradle-command> [args...]" >&2
  exit 2
fi
retry_command=("$@")

max_attempts=${GRADLE_FLAKE_RETRY_MAX_ATTEMPTS:-3}
case "$max_attempts" in
  ''|*[!0-9]*|0)
    echo "GRADLE_FLAKE_RETRY_MAX_ATTEMPTS must be a positive integer, got: $max_attempts" >&2
    exit 2
    ;;
esac

log=${GRADLE_FLAKE_RETRY_LOG:-${RUNNER_TEMP:-/tmp}/gradle-build.log}

# flake_reason <log-file> prints the matched flake reason, or nothing when the
# log does not match a known rerun-cured signature.
flake_reason() {
  if grep -q 'IncrementalSplitterRunnable' "$1"; then
    echo 'IncrementalSplitterRunnable packaging crash (#4098)'
    return 0
  fi
  if grep -q 'DexArchiveMergerException' "$1" && \
      grep -q 'java.lang.OutOfMemoryError: Java heap space' "$1"; then
    echo 'D8 dex-merge heap exhaustion (#4351)'
    return 0
  fi
  return 0
}

ok=0
reason=''
for attempt in $(seq 1 "$max_attempts"); do
  if "${retry_command[@]}" 2>&1 | tee "$log"; then
    ok=1
    break
  fi
  reason="$(flake_reason "$log")"
  if [ -z "$reason" ]; then
    echo "::error::Gradle build failed without a known flake signature; not retrying (issues #4098, #4351)"
    exit 1
  fi
  echo "Gradle flake retry: ${reason} on attempt ${attempt}/${max_attempts}"
  if [ "$attempt" -lt "$max_attempts" ]; then
    sleep "${GRADLE_FLAKE_RETRY_BACKOFF:-$((attempt * 10))}"
  fi
done

if [ "$ok" != 1 ]; then
  echo "::error::Gradle build failed after ${max_attempts} attempts (${reason:-no known flake signature}); see issues #4098, #4351"
  exit 1
fi
exit 0
