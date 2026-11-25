#!/usr/bin/env bash
# Stub-gh unit tests for scripts/beta_train_ci_gate.sh.
#
# The gate classifies main CI coverage for a candidate head. These tests drive
# it with fixed fixtures instead of live GitHub data so the four-state
# classification and the docs-only inheritance window stay deterministic.
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
gate="${root}/scripts/beta_train_ci_gate.sh"
paths="${root}/.github/workflows/ci.yml"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failures=0
case_index=0
case_dir=''

new_case() { # name
  case_index=$((case_index + 1))
  case_dir="${work}/case-${case_index}"
  mkdir -p "${case_dir}/bin"
  export STUB_DIR="$case_dir"
  unset STUB_COMPARE_EXIT
  cat > "${case_dir}/bin/gh" <<'STUB'
#!/usr/bin/env bash
args="$*"
case "$args" in
  *"run list"*"--status success"*) jq -r '.[0].headSha // empty' "${STUB_DIR}/green.json" ;;
  *"run list"*"--commit"*) cat "${STUB_DIR}/runs.json" ;;
  *"api"*"/compare/"*)
    if [ -n "${STUB_COMPARE_EXIT:-}" ]; then
      printf 'compare unavailable\n' >&2
      exit "${STUB_COMPARE_EXIT}"
    fi
    cat "${STUB_DIR}/compare.json"
    ;;
  *) printf 'unexpected gh args: %s\n' "$args" >&2; exit 64 ;;
esac
STUB
  chmod +x "${case_dir}/bin/gh"
}

run_gate() { # sha
  SHA="$1" GITHUB_REPOSITORY=holon-run/holon CI_PATHS_FILE="$paths" \
    PATH="${case_dir}/bin:${PATH}" bash "$gate"
}

field() { # output key
  printf '%s\n' "$1" | sed -n "s/^$2=//p"
}

assert_eq() { # label expected actual
  if [ "$2" != "$3" ]; then
    printf 'FAIL %s: expected %q, got %q\n' "$1" "$2" "$3" >&2
    failures=$((failures + 1))
  fi
}

assert_contains() { # label haystack needle
  case "$2" in
    *"$3"*) ;;
    *)
      printf 'FAIL %s: %q does not contain %q\n' "$1" "$2" "$3" >&2
      failures=$((failures + 1))
      ;;
  esac
}

assert_not_contains() { # label haystack needle
  case "$2" in
    *"$3"*)
      printf 'FAIL %s: %q unexpectedly contains %q\n' "$1" "$2" "$3" >&2
      failures=$((failures + 1))
      ;;
  esac
}

green_sha='39e5b5639f5be7781bcf57fdf3df6d35452ab7e3'
head_sha='2038a5570360f361e705e320d5e06cb41392aadb'

# 1. A run that exists but has not completed is pending, never a failure.
new_case pending
printf '[{"status":"in_progress","conclusion":""}]\n' > "${case_dir}/runs.json"
out="$(run_gate "$head_sha")"
assert_eq 'pending decision' 'skip' "$(field "$out" decision)"
assert_eq 'pending state' 'pending' "$(field "$out" state)"
assert_contains 'pending message' "$(field "$out" message)" 'still running'
assert_not_contains 'pending must not blame a failure' "$(field "$out" message)" 'fix the failure'

# 2. Completed run with an empty conclusion is fail-closed, not success.
new_case completed-empty-conclusion
printf '[{"status":"completed","conclusion":""}]\n' > "${case_dir}/runs.json"
out="$(run_gate "$head_sha")"
assert_eq 'empty conclusion decision' 'skip' "$(field "$out" decision)"
assert_eq 'empty conclusion state' 'blocked' "$(field "$out" state)"

# 3. Green run for the candidate head passes unchanged.
new_case success
printf '[{"status":"completed","conclusion":"success"}]\n' > "${case_dir}/runs.json"
out="$(run_gate "$head_sha")"
assert_eq 'success decision' 'pass' "$(field "$out" decision)"
assert_eq 'success state' 'success' "$(field "$out" state)"
assert_eq 'success passed clause' 'main CI success' "$(field "$out" passed)"
assert_eq 'success notice' '' "$(field "$out" notice)"

# 4. Red run keeps the legacy failure wording.
new_case failure
printf '[{"status":"completed","conclusion":"failure"}]\n' > "${case_dir}/runs.json"
out="$(run_gate "$head_sha")"
assert_eq 'failure decision' 'skip' "$(field "$out" decision)"
assert_eq 'failure state' 'blocked' "$(field "$out" state)"
assert_contains 'failure message' "$(field "$out" message)" 'is failure; fix the failure'

# 5. The 2026-11-25 incident: docs-only window inherits the last green main CI.
new_case docs-only-inherit
printf '[]\n' > "${case_dir}/runs.json"
printf '[{"headSha":"%s"}]\n' "$green_sha" > "${case_dir}/green.json"
cat > "${case_dir}/compare.json" <<JSON
{
  "status": "ahead",
  "ahead_by": 1,
  "total_commits": 1,
  "files": [
    {"filename": "docs/website/.tools/README.md"},
    {"filename": "docs/website/.tools/package.json"},
    {"filename": "docs/website/.tools/package-lock.json"}
  ]
}
JSON
out="$(run_gate "$head_sha")"
assert_eq 'inherit decision' 'pass' "$(field "$out" decision)"
assert_eq 'inherit state' 'missing' "$(field "$out" state)"
assert_contains 'inherit notice' "$(field "$out" notice)" "inherited from ${green_sha}, 3 docs-only files"
assert_contains 'inherit passed clause' "$(field "$out" passed)" 'docs-only window: 3 file(s), all outside CI push paths'

# 6. A window that carries CI-covered code is a real gap, not an inheritance.
new_case window-contains-code
printf '[]\n' > "${case_dir}/runs.json"
printf '[{"headSha":"%s"}]\n' "$green_sha" > "${case_dir}/green.json"
cat > "${case_dir}/compare.json" <<JSON
{"status": "ahead", "ahead_by": 2, "files": [
  {"filename": "docs/website/.tools/README.md"},
  {"filename": "src/runtime/turn.rs"}
]}
JSON
out="$(run_gate "$head_sha")"
assert_eq 'code window decision' 'skip' "$(field "$out" decision)"
assert_contains 'code window reason' "$(field "$out" message)" 'window file src/runtime/turn.rs is covered by CI push path src/**'
assert_not_contains 'code window must not blame a failure' "$(field "$out" message)" 'fix the failure'

# 7. A generated docs artifact inside ci.yml paths can never reach this branch.
new_case window-contains-generated-reference
printf '[]\n' > "${case_dir}/runs.json"
printf '[{"headSha":"%s"}]\n' "$green_sha" > "${case_dir}/green.json"
cat > "${case_dir}/compare.json" <<JSON
{"status": "ahead", "ahead_by": 1, "files": [
  {"filename": "docs/website/reference/openapi.json"}
]}
JSON
out="$(run_gate "$head_sha")"
assert_eq 'reference window decision' 'skip' "$(field "$out" decision)"
assert_contains 'reference window reason' "$(field "$out" message)" 'covered by CI push path docs/website/reference/openapi.json'

# 8. Without a green main CI there is nothing to inherit.
new_case no-green-run
printf '[]\n' > "${case_dir}/runs.json"
printf '[]\n' > "${case_dir}/green.json"
out="$(run_gate "$head_sha")"
assert_eq 'no green decision' 'skip' "$(field "$out" decision)"
assert_eq 'no green state' 'missing' "$(field "$out" state)"
assert_contains 'no green reason' "$(field "$out" message)" 'no green CI on main to inherit'

# 9. A failed compare request is not evidence of a docs-only window.
new_case compare-request-failed
printf '[]\n' > "${case_dir}/runs.json"
printf '[{"headSha":"%s"}]\n' "$green_sha" > "${case_dir}/green.json"
export STUB_COMPARE_EXIT=1
out="$(run_gate "$head_sha")"
assert_eq 'compare failure decision' 'skip' "$(field "$out" decision)"
assert_contains 'compare failure reason' "$(field "$out" message)" 'compare request failed'

# 10. A compare response without a file list cannot prove completeness.
new_case compare-files-null
printf '[]\n' > "${case_dir}/runs.json"
printf '[{"headSha":"%s"}]\n' "$green_sha" > "${case_dir}/green.json"
printf '{"status":"ahead","ahead_by":1,"files":null}\n' > "${case_dir}/compare.json"
out="$(run_gate "$head_sha")"
assert_eq 'null files decision' 'skip' "$(field "$out" decision)"
assert_contains 'null files reason' "$(field "$out" message)" 'compare returned no file list'

# 11. A file list at the compare API cap is treated as truncated.
new_case compare-files-truncated
printf '[]\n' > "${case_dir}/runs.json"
printf '[{"headSha":"%s"}]\n' "$green_sha" > "${case_dir}/green.json"
jq -n '{status: "ahead", ahead_by: 300, files: [range(300) | {filename: ("docs/website/.tools/f\(.) .md")}]}' > "${case_dir}/compare.json"
out="$(run_gate "$head_sha")"
assert_eq 'truncated decision' 'skip' "$(field "$out" decision)"
assert_contains 'truncated reason' "$(field "$out" message)" 'truncated at 300 files'

# 12. A window that is not strictly ahead cannot be inherited.
new_case compare-not-ahead
printf '[]\n' > "${case_dir}/runs.json"
printf '[{"headSha":"%s"}]\n' "$green_sha" > "${case_dir}/green.json"
printf '{"status":"identical","ahead_by":0,"files":[]}\n' > "${case_dir}/compare.json"
out="$(run_gate "$head_sha")"
assert_eq 'not-ahead decision' 'skip' "$(field "$out" decision)"
assert_contains 'not-ahead reason' "$(field "$out" message)" 'compare status identical'

if [ "$failures" -ne 0 ]; then
  printf '%d beta-train CI gate test(s) failed\n' "$failures" >&2
  exit 1
fi
printf 'beta-train CI gate: %d case(s) passed\n' "$case_index"
