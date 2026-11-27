#!/usr/bin/env bash
# Stub-based unit tests for scripts/gradle_flake_retry.sh.
#
# The wrapper retries a Gradle invocation only for known rerun-cured flake
# signatures (#4098, #4351). These tests drive it with a stub command that
# replays fixed log fixtures, so retry classification, the no-retry fast exit,
# and attempt exhaustion stay deterministic without a real Gradle build.
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
retry="${root}/scripts/gradle_flake_retry.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fixtures="${work}/fixtures"
mkdir -p "$fixtures"

cat > "${fixtures}/incremental-splitter.log" <<'EOF'
FAILURE: Build failed with an exception.
* What went wrong:
Execution failed for task ':app:packageDirectRelease'.
> java.lang.RuntimeException: AGP incremental packaging failure in PackageAndroidArtifact$IncrementalSplitterRunnable
EOF

cat > "${fixtures}/d8-merge-oom.log" <<'EOF'
ERROR: D8: java.lang.OutOfMemoryError: Java heap space
com.android.builder.dexing.DexArchiveMergerException: Error while merging dex archives
Caused by: com.android.tools.r8.CompilationFailedException: Compilation failed to complete
Caused by: java.lang.OutOfMemoryError: Java heap space
EOF

cat > "${fixtures}/d8-merge-no-oom.log" <<'EOF'
com.android.builder.dexing.DexArchiveMergerException: Error while merging dex archives
Caused by: com.android.tools.r8.CompilationFailedException: Compilation failed to complete
Caused by: com.android.tools.r8.DiagnosticError: Duplicate class foo.Bar found in two modules
EOF

cat > "${fixtures}/oom-no-dex-merge.log" <<'EOF'
> Task :app:testPlayDebugUnitTest FAILED
java.lang.OutOfMemoryError: Java heap space
	at org.junit.runners.ParentRunner.run(ParentRunner.java:356)
EOF

cat > "${fixtures}/unknown-failure.log" <<'EOF'
FAILURE: Build failed with an exception.
* What went wrong:
Execution failed for task ':app:testPlayDebugUnitTest'.
> Assertion failed in test
EOF

failures=0
case_index=0
case_dir=''
case_output=''

new_case() { # name
  case_index=$((case_index + 1))
  case_dir="${work}/case-${case_index}"
  mkdir -p "${case_dir}/bin"
  : > "${case_dir}/plan"
}

plan() { # plan-steps, one argument per invocation
  local step
  for step in "$@"; do
    printf '%s\n' "$step" >> "${case_dir}/plan"
  done
}

cat > "${work}/gradlew-stub" <<'STUB'
#!/usr/bin/env bash
set -u
count_file="${STUB_DIR}/invocations"
n=$(( $(cat "$count_file" 2>/dev/null || echo 0) + 1 ))
printf '%s\n' "$n" > "$count_file"
step="$(sed -n "${n}p" "${STUB_DIR}/plan")"
if [ -z "$step" ]; then
  echo "stub: plan exhausted at invocation ${n}" >&2
  exit 64
fi
case "$step" in
  ok) echo 'BUILD SUCCESSFUL in stub' ;;
  4098) cat "${FIXTURES}/incremental-splitter.log" ;;
  4351) cat "${FIXTURES}/d8-merge-oom.log" ;;
  d8-only) cat "${FIXTURES}/d8-merge-no-oom.log" ;;
  oom-only) cat "${FIXTURES}/oom-no-dex-merge.log" ;;
  unknown) cat "${FIXTURES}/unknown-failure.log" ;;
  *) echo "stub: unexpected plan step: ${step}" >&2; exit 64 ;;
esac
if [ "$step" = ok ]; then
  exit 0
fi
exit 1
STUB
chmod +x "${work}/gradlew-stub"

run_retry() { # expected-exit-code
  local expected=$1
  STUB_DIR="$case_dir" \
  FIXTURES="$fixtures" \
  GRADLE_FLAKE_RETRY_BACKOFF=0 \
  GRADLE_FLAKE_RETRY_LOG="${case_dir}/gradle-build.log" \
    bash "$retry" "${work}/gradlew-stub" > "${case_dir}/output" 2>&1
  local status=$?
  case_output="$(cat "${case_dir}/output")"
  assert_eq 'exit code' "$expected" "$status"
}

invocations() {
  cat "${case_dir}/invocations" 2>/dev/null || echo 0
}

assert_eq() { # label expected actual
  if [ "$2" != "$3" ]; then
    printf 'FAIL %s: expected %q, got %q\n' "$1" "$2" "$3" >&2
    failures=$((failures + 1))
  fi
}

assert_contains() { # label needle
  if ! printf '%s' "$case_output" | grep -qF "$2"; then
    printf 'FAIL %s: output does not contain: %q\n' "$1" "$2" >&2
    failures=$((failures + 1))
  fi
}

assert_not_contains() { # label needle
  if printf '%s' "$case_output" | grep -qF "$2"; then
    printf 'FAIL %s: output unexpectedly contains: %q\n' "$1" "$2" >&2
    failures=$((failures + 1))
  fi
}

result() { # name
  printf '%s\n' "$1"
}

# 1. Success on the first attempt: no retry.
new_case 'success-first-attempt'
plan ok
run_retry 0
assert_eq 'invocations' 1 "$(invocations)"
assert_contains 'success output' 'BUILD SUCCESSFUL in stub'
assert_not_contains 'no retry error' 'not retrying'

# 2. #4098 signature hits, retry succeeds.
new_case 'retry-4098-then-success'
plan 4098 ok
run_retry 0
assert_eq 'invocations' 2 "$(invocations)"
assert_contains 'retry reason' 'IncrementalSplitterRunnable packaging crash (#4098)'
assert_contains 'attempt count' 'attempt 1/3'

# 3. #4351 signature hits (both parts), retry succeeds.
new_case 'retry-4351-then-success'
plan 4351 ok
run_retry 0
assert_eq 'invocations' 2 "$(invocations)"
assert_contains 'retry reason' 'D8 dex-merge heap exhaustion (#4351)'

# 4. Dex merge failure without heap OOM is not a known flake: fast exit.
new_case 'no-retry-dex-merge-without-oom'
plan d8-only
run_retry 1
assert_eq 'invocations' 1 "$(invocations)"
assert_contains 'not retrying' 'not retrying'

# 5. Heap OOM outside dex merging (e.g. a test JVM) is not a known flake.
new_case 'no-retry-oom-without-dex-merge'
plan oom-only
run_retry 1
assert_eq 'invocations' 1 "$(invocations)"
assert_contains 'not retrying' 'not retrying'

# 6. Unknown failure is not retried.
new_case 'no-retry-unknown-failure'
plan unknown
run_retry 1
assert_eq 'invocations' 1 "$(invocations)"
assert_contains 'not retrying' 'not retrying'

# 7. Persistent #4351 flake exhausts the attempt budget.
new_case 'exhaust-attempts-4351'
plan 4351 4351 4351
run_retry 1
assert_eq 'invocations' 3 "$(invocations)"
assert_contains 'exhaustion' 'failed after 3 attempts'
assert_contains 'exhaustion reason' 'D8 dex-merge heap exhaustion (#4351)'

# 8. Different known signatures across attempts still retry until success.
new_case 'mixed-signatures-then-success'
plan 4098 4351 ok
run_retry 0
assert_eq 'invocations' 3 "$(invocations)"
assert_contains 'first retry' 'attempt 1/3'
assert_contains 'second retry' 'attempt 2/3'

printf 'gradle_flake_retry tests: %d case(s), %d failure(s)\n' "$case_index" "$failures"
if [ "$failures" -gt 0 ]; then
  exit 1
fi
