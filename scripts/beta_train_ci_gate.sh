#!/usr/bin/env bash
# Beta train preflight: classify main CI coverage for the candidate head.
#
# Emits KEY=VALUE lines on stdout:
#   decision=pass|skip
#   state=success|pending|blocked|missing
#   message=<skip reason, empty when decision=pass>
#   notice=<notice text for an inherited success, empty otherwise>
#   passed=<clause for the "Preflight passed" line>
#
# Fail-closed contract: the candidate head passes only when its own main CI run
# is green, or when there is provably no CI run for it because the whole window
# since the last green main CI touches nothing that ci.yml's push paths cover.
# attest/tag/Release handoff are outside this script's scope.
#
# Drift guard: this reads ci.yml's on.push.paths at runtime instead of keeping a
# second docs list here; an empty or unreadable list means "cannot prove
# docs-only" and skips. docs/website/.tools/** is deliberately not special-cased:
# docs-site.yml validates docs/website/** and release.yml never builds the
# website, so the binary release chain is unaffected. If the website ever joins
# the release-critical path, this assumption must be revisited.
set -uo pipefail

repo="${GITHUB_REPOSITORY:-}"
sha="${SHA:-}"
paths_file="${CI_PATHS_FILE:-.github/workflows/ci.yml}"

if [ -z "$repo" ] || [ -z "$sha" ]; then
  printf 'beta_train_ci_gate: GITHUB_REPOSITORY and SHA are required\n' >&2
  exit 2
fi

emit() { # decision state message notice passed
  printf 'decision=%s\nstate=%s\nmessage=%s\nnotice=%s\npassed=%s\n' "$1" "$2" "$3" "$4" "$5"
}

skip() { # state message
  emit skip "$1" "$2" "" ""
  exit 0
}

pattern_to_regex() { # github path filter -> extended regex
  local p="$1" out='^' i=0 c next
  while [ "$i" -lt "${#p}" ]; do
    c="${p:$i:1}"
    next="${p:$((i + 1)):1}"
    if [ "$c" = '*' ] && [ "$next" = '*' ]; then
      out="${out}.*"
      i=$((i + 2))
      continue
    elif [ "$c" = '*' ]; then
      out="${out}[^/]*"
    elif [ "$c" = '?' ]; then
      out="${out}[^/]"
    else
      case "$c" in
        [a-zA-Z0-9] | '/' | '-' | '_') out="${out}${c}" ;;
        *) out="${out}\\${c}" ;;
      esac
    fi
    i=$((i + 1))
  done
  printf '%s$' "$out"
}

runs="$(gh run list --repo "$repo" --workflow CI --branch main --commit "$sha" \
  --limit 1 --json status,conclusion)" || runs=""
if [ -z "$runs" ]; then
  skip missing "main CI lookup for ${sha} failed; failing closed without a release"
fi

parsed="$(printf '%s' "$runs" | jq -r '
  if type != "array" then ["invalid", "", ""]
  elif length == 0 then ["missing", "", ""]
  else ["run", (.[0].status // ""), (.[0].conclusion // "")]
  end | @tsv')" || parsed=""
if [ -z "$parsed" ]; then
  skip missing "main CI lookup for ${sha} returned an unexpected payload; failing closed"
fi
IFS=$'\t' read -r kind run_status run_conclusion <<<"$parsed"

if [ "$kind" = "run" ]; then
  # Four-state classification keys on run.status first: conclusion is only
  # meaningful once the run completed, and an empty conclusion must never be
  # rendered as a failure.
  if [ "$run_status" != "completed" ]; then
    skip pending "main CI on ${sha} is still running; the train retries on the next dispatch"
  fi
  if [ "$run_conclusion" = "success" ]; then
    emit pass success "" "" "main CI success"
    exit 0
  fi
  skip blocked "main CI on ${sha} is ${run_conclusion:-unknown}; fix the failure or rule out an infrastructure flake before the next train"
fi

# No run at all: the only permissive branch is a provable docs-only window.
green_sha="$(gh run list --repo "$repo" --workflow CI --branch main --status success \
  --limit 1 --json headSha --jq '.[0].headSha // empty')" || green_sha=""
if [ -z "$green_sha" ]; then
  skip missing "main CI missing on ${sha} and no green CI on main to inherit"
fi

compare_json="$(gh api "repos/${repo}/compare/${green_sha}...${sha}")" || compare_json=""
if [ -z "$compare_json" ]; then
  skip missing "main CI missing on ${sha}; cannot prove a docs-only window since ${green_sha} (compare request failed)"
fi

summary="$(printf '%s' "$compare_json" | jq -r '
  if type != "object" then ["invalid", "", ""]
  else [
    (.status // ""),
    ((.ahead_by // 0) | tostring),
    (if (.files == null) then "null" else ((.files | length) | tostring) end)
  ]
  end | @tsv')" || summary=""
if [ -z "$summary" ]; then
  skip missing "main CI missing on ${sha}; cannot prove a docs-only window since ${green_sha} (unexpected compare payload)"
fi
IFS=$'\t' read -r compare_status ahead_by file_count <<<"$summary"

if [ "$compare_status" != "ahead" ]; then
  skip missing "main CI missing on ${sha}; cannot prove a docs-only window since ${green_sha} (compare status ${compare_status:-unknown}, ahead_by ${ahead_by:-0})"
fi
if [ "${ahead_by:-0}" -le 0 ] 2>/dev/null; then
  skip missing "main CI missing on ${sha}; cannot prove a docs-only window since ${green_sha} (no commits ahead)"
fi
if [ "$file_count" = "null" ]; then
  skip missing "main CI missing on ${sha}; cannot prove a docs-only window since ${green_sha} (compare returned no file list)"
fi
if [ "${file_count:-0}" -ge 300 ] 2>/dev/null; then
  skip missing "main CI missing on ${sha}; cannot prove a docs-only window since ${green_sha} (compare file list truncated at ${file_count} files)"
fi
if [ "${file_count:-0}" -le 0 ] 2>/dev/null; then
  skip missing "main CI missing on ${sha}; cannot prove a docs-only window since ${green_sha} (compare returned no changed files)"
fi

mapfile -t patterns < <(awk '
  {
    if ($0 ~ /^on:[[:space:]]*$/) { on=1; push=0; paths=0; next }
    if ($0 ~ /^[^[:space:]#]/) { on=0; push=0; paths=0; next }
    if (on && $0 ~ /^  push:[[:space:]]*$/) { push=1; paths=0; next }
    if (push && $0 ~ /^    paths:[[:space:]]*$/) { paths=1; next }
    if (paths) {
      if ($0 ~ /^      -[[:space:]]/) { print; next }
      if ($0 ~ /^[[:space:]]*$/) next
      paths=0
    }
  }' "$paths_file" 2>/dev/null | sed -e 's/^      -[[:space:]]*//' -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'\$//")
if [ "${#patterns[@]}" -eq 0 ]; then
  skip missing "main CI missing on ${sha}; cannot prove a docs-only window since ${green_sha} (ci.yml push path list is empty or unreadable)"
fi

while IFS= read -r file; do
  [ -z "$file" ] && continue
  for pattern in "${patterns[@]}"; do
    if [[ "$file" =~ $(pattern_to_regex "$pattern") ]]; then
      skip missing "main CI missing on ${sha}; cannot prove a docs-only window since ${green_sha} (window file ${file} is covered by CI push path ${pattern})"
    fi
  done
done < <(printf '%s' "$compare_json" | jq -r '.files[]?.filename')

emit pass missing "" \
  "main CI success (inherited from ${green_sha}, ${file_count} docs-only files)" \
  "main CI on ${sha} inherited from ${green_sha} (docs-only window: ${file_count} file(s), all outside CI push paths)"
