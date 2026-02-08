#!/bin/bash
set -euo pipefail

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

resolve_repo() {
  if [[ -n "${PULSE_REPO:-}" ]]; then
    printf '%s\n' "$PULSE_REPO"
    return
  fi
  if [[ $# -ge 1 && -n "${1:-}" ]]; then
    printf '%s\n' "$1"
    return
  fi

  local origin
  origin="$(git remote get-url origin 2>/dev/null || true)"
  if [[ -n "$origin" ]]; then
    origin="${origin%.git}"
    origin="${origin#git@github.com:}"
    origin="${origin#https://github.com/}"
    if [[ "$origin" == */* ]]; then
      printf '%s\n' "$origin"
      return
    fi
  fi

  echo "failed to resolve repo. pass owner/repo as arg or set PULSE_REPO" >&2
  exit 2
}

require_cmd gh
require_cmd jq

REPO="$(resolve_repo "$@")"
INCLUDE_CLOSED="${PULSE_INCLUDE_CLOSED:-false}"
MAX_ISSUES="${PULSE_MAX_ISSUES:-500}"
MAX_PRS="${PULSE_MAX_PRS:-500}"
STALE_DAYS="${PULSE_STALE_DAYS:-14}"
MAX_AGE_MINUTES="${PULSE_MAX_AGE_MINUTES:-10}"
FORCE_REFRESH="${PULSE_FORCE_REFRESH:-false}"
SPLIT_ISSUES="${PULSE_SPLIT_ISSUES:-true}"
KEEP_RAW_ISSUES="${PULSE_KEEP_RAW_ISSUES:-false}"
SPLIT_PRS="${PULSE_SPLIT_PRS:-true}"
KEEP_RAW_PRS="${PULSE_KEEP_RAW_PRS:-false}"

if [[ "$INCLUDE_CLOSED" == "true" ]]; then
  ISSUE_STATE="all"
else
  ISSUE_STATE="open"
fi

HOME_DIR="${HOME:-$PWD}"
PULSE_BASE_DIR="${PULSE_BASE_DIR:-$HOME_DIR/.holon/project-pulse/$REPO}"
PULSE_OUT_DIR="${PULSE_OUT_DIR:-$PULSE_BASE_DIR}"
PULSE_STATE_DIR="${PULSE_STATE_DIR:-$PULSE_BASE_DIR}"

mkdir -p "$PULSE_OUT_DIR" "$PULSE_STATE_DIR"

ISSUES_JSON="$PULSE_OUT_DIR/issues.json"
ISSUES_INDEX_JSON="$PULSE_OUT_DIR/issues-index.json"
ISSUES_DIR="$PULSE_OUT_DIR/issues"
PRS_JSON="$PULSE_OUT_DIR/prs.json"
PRS_INDEX_JSON="$PULSE_OUT_DIR/prs-index.json"
PRS_DIR="$PULSE_OUT_DIR/prs"
REPORT_JSON="$PULSE_OUT_DIR/report.json"
REPORT_MD="$PULSE_OUT_DIR/report.md"
SYNC_STATE="$PULSE_STATE_DIR/sync-state.json"
WORK_ISSUES_JSON="$(mktemp /tmp/project-pulse-issues-XXXXXX.json)"
WORK_PRS_JSON="$(mktemp /tmp/project-pulse-prs-XXXXXX.json)"

PREV_MAX_UPDATED=""
if [[ -f "$SYNC_STATE" ]]; then
  PREV_MAX_UPDATED="$(jq -r '.last_max_updated_at // ""' "$SYNC_STATE")"
fi

if [[ "$FORCE_REFRESH" != "true" && -f "$REPORT_JSON" && -f "$ISSUES_INDEX_JSON" && -f "$PRS_INDEX_JSON" ]]; then
  REPORT_AGE_MINUTES="$(jq -r 'if .metadata.fetched_at then ((now - (.metadata.fetched_at | fromdateiso8601)) / 60 | floor) else 999999 end' "$REPORT_JSON" 2>/dev/null || echo "999999")"
  if [[ "$REPORT_AGE_MINUTES" =~ ^[0-9]+$ ]] && [[ "$REPORT_AGE_MINUTES" -le "$MAX_AGE_MINUTES" ]]; then
    echo "project-pulse cache hit"
    echo "cache age:   ${REPORT_AGE_MINUTES}m (max ${MAX_AGE_MINUTES}m)"
    echo "issues dir:  $ISSUES_DIR"
    echo "issues idx:  $ISSUES_INDEX_JSON"
    echo "prs dir:     $PRS_DIR"
    echo "prs idx:     $PRS_INDEX_JSON"
    echo "report json: $REPORT_JSON"
    echo "report md:   $REPORT_MD"
    echo "sync state:  $SYNC_STATE"
    rm -f "$WORK_ISSUES_JSON" "$WORK_PRS_JSON"
    exit 0
  fi
fi

# GitHub is source-of-truth; local files are cache/snapshots for fast analysis.
gh issue list \
  --repo "$REPO" \
  --state "$ISSUE_STATE" \
  --limit "$MAX_ISSUES" \
  --json number,title,state,labels,assignees,createdAt,updatedAt,comments,milestone,url > "$WORK_ISSUES_JSON"

gh pr list \
  --repo "$REPO" \
  --state "$ISSUE_STATE" \
  --limit "$MAX_PRS" \
  --json number,title,state,isDraft,mergeStateStatus,reviewDecision,statusCheckRollup,createdAt,updatedAt,url,labels,assignees,baseRefName,headRefName > "$WORK_PRS_JSON"

if [[ "$SPLIT_ISSUES" == "true" ]]; then
  mkdir -p "$ISSUES_DIR"
  jq -c '.[]' "$WORK_ISSUES_JSON" | while IFS= read -r issue; do
    issue_number="$(jq -r '.number' <<<"$issue")"
    printf '%s\n' "$issue" > "$ISSUES_DIR/$issue_number.json"
  done

  jq '
    [
      .[] | {
        number,
        title,
        state,
        updatedAt,
        url,
        priority: ((.labels | map(.name) | map(select(startswith("priority:"))))[0] // "priority:none")
      }
    ]' "$WORK_ISSUES_JSON" > "$ISSUES_INDEX_JSON"
fi

if [[ "$SPLIT_ISSUES" != "true" || "$KEEP_RAW_ISSUES" == "true" ]]; then
  cp "$WORK_ISSUES_JSON" "$ISSUES_JSON"
fi

if [[ "$SPLIT_PRS" == "true" ]]; then
  mkdir -p "$PRS_DIR"
  jq -c '.[]' "$WORK_PRS_JSON" | while IFS= read -r pr; do
    pr_number="$(jq -r '.number' <<<"$pr")"
    printf '%s\n' "$pr" > "$PRS_DIR/$pr_number.json"
  done

  jq '
    [
      .[] | {
        number,
        title,
        state,
        isDraft,
        mergeStateStatus,
        reviewDecision,
        updatedAt,
        url
      }
    ]' "$WORK_PRS_JSON" > "$PRS_INDEX_JSON"
fi

if [[ "$SPLIT_PRS" != "true" || "$KEEP_RAW_PRS" == "true" ]]; then
  cp "$WORK_PRS_JSON" "$PRS_JSON"
fi

MAX_UPDATED="$(jq -r 'if length == 0 then "" else (max_by(.updatedAt).updatedAt) end' "$WORK_ISSUES_JSON")"

if [[ -z "$PREV_MAX_UPDATED" ]]; then
  UPDATED_SINCE_LAST="$(jq 'length' "$WORK_ISSUES_JSON")"
else
  UPDATED_SINCE_LAST="$(jq --arg prev "$PREV_MAX_UPDATED" '[.[] | select(.updatedAt > $prev)] | length' "$WORK_ISSUES_JSON")"
fi

jq \
  --arg repo "$REPO" \
  --arg issue_state "$ISSUE_STATE" \
  --arg pr_state "$ISSUE_STATE" \
  --arg stale_days "$STALE_DAYS" \
  --arg previous_max_updated "$PREV_MAX_UPDATED" \
  --arg current_max_updated "$MAX_UPDATED" \
  --arg source_of_truth "github" \
  --arg fetched_at "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" \
  --argjson updated_since_last "$UPDATED_SINCE_LAST" \
  --slurpfile prs "$WORK_PRS_JSON" \
  '
  def checks_failed($pr):
    any($pr.statusCheckRollup[]?;
      ((.__typename == "CheckRun")
       and ((.conclusion // "" | ascii_upcase) as $c
       | ($c == "FAILURE" or $c == "TIMED_OUT" or $c == "CANCELLED" or $c == "ACTION_REQUIRED" or $c == "STARTUP_FAILURE")))
      or
      ((.__typename == "StatusContext")
       and (((.state // "" | ascii_upcase) == "ERROR") or ((.state // "" | ascii_upcase) == "FAILURE")))
    );
  def pcount($name): [ .[] | select(.labels | map(.name) | index($name)) ] | length;
  def stale_issues($days):
    [ .[]
      | select(.state == "OPEN")
      | ((now - (.updatedAt | fromdateiso8601)) / 86400 | floor) as $age
      | select($age >= ($days|tonumber))
      | {
          number,
          title,
          updatedAt,
          url,
          age_days: $age,
          priority: ((.labels | map(.name) | map(select(startswith("priority:"))))[0] // "priority:none")
        }
    ];
  def stale_prs($prs; $days):
    [ $prs[]
      | select(.state == "OPEN")
      | ((now - (.updatedAt | fromdateiso8601)) / 86400 | floor) as $age
      | select($age >= ($days|tonumber))
      | {number, title, updatedAt, url, age_days: $age, merge_state: .mergeStateStatus, review_decision: .reviewDecision}
    ];
  def critical_issues:
    [ .[]
      | select(.state == "OPEN")
      | select(.labels | map(.name) | index("priority:critical"))
      | {number, title, updatedAt, url}
    ];
  def failing_open_prs($prs):
    [ $prs[]
      | select(.state == "OPEN" and checks_failed(.))
      | {number, title, updatedAt, url}
    ];
  def changes_requested_open_prs($prs):
    [ $prs[]
      | select(.state == "OPEN" and (.reviewDecision // "") == "CHANGES_REQUESTED")
      | {number, title, updatedAt, url}
    ];
  def next_actions($repo; $issues; $prs):
    (
      (
        [ failing_open_prs($prs)[] | {
            action: "fix_pr",
            target: ($repo + "#" + (.number|tostring)),
            reason: "Open PR has failing checks and should be stabilized first.",
            priority: "critical"
          } ][0:5]
      )
      +
      (
        [ changes_requested_open_prs($prs)[] | {
            action: "fix_pr",
            target: ($repo + "#" + (.number|tostring)),
            reason: "PR has CHANGES_REQUESTED and should be addressed to unblock review flow.",
            priority: "high"
          } ][0:5]
      )
      +
      (
        [ critical_issues[] | {
            action: "solve_issue",
            target: ($repo + "#" + (.number|tostring)),
            reason: "Issue is priority:critical and is part of the delivery critical path.",
            priority: "critical"
          } ][0:5]
      )
    ) as $actions
    | (if ($actions | length) == 0 then
        [{
          action: "wait",
          target: $repo,
          reason: "No urgent pulse signals detected; keep monitoring for new events.",
          priority: "low"
        }]
      else
        ($actions | unique_by(.action + "|" + .target))
      end);
  {
    metadata: {
      repo: $repo,
      source_of_truth: $source_of_truth,
      issue_state: $issue_state,
      pr_state: $pr_state,
      fetched_at: $fetched_at,
      previous_max_updated: $previous_max_updated,
      current_max_updated: $current_max_updated,
      updated_since_last: $updated_since_last
    },
    totals: {
      issues: length,
      open: [ .[] | select(.state == "OPEN") ] | length,
      closed: [ .[] | select(.state == "CLOSED") ] | length,
      priority_critical: pcount("priority:critical"),
      priority_high: pcount("priority:high"),
      priority_medium: pcount("priority:medium"),
      priority_low: pcount("priority:low")
    },
    pr_totals: {
      prs: ($prs[0] | length),
      open: ($prs[0] | map(select(.state == "OPEN")) | length),
      closed: ($prs[0] | map(select(.state == "CLOSED")) | length),
      merged: ($prs[0] | map(select(.state == "MERGED")) | length),
      drafts: ($prs[0] | map(select(.state == "OPEN" and .isDraft == true)) | length),
      failing_checks: ($prs[0] | map(select(.state == "OPEN" and checks_failed(.))) | length),
      changes_requested: ($prs[0] | map(select(.state == "OPEN" and (.reviewDecision // "") == "CHANGES_REQUESTED")) | length)
    },
    stale_issues: stale_issues($stale_days),
    stalled_prs: stale_prs($prs[0]; $stale_days),
    failing_prs: (
      [
        $prs[0][]
        | select(.state == "OPEN" and checks_failed(.))
        | {number, title, updatedAt, url, merge_state: .mergeStateStatus}
      ]
    ),
    ready_to_merge_candidates: (
      [
        $prs[0][]
        | select(.state == "OPEN")
        | select(.isDraft != true)
        | select((.reviewDecision // "") == "APPROVED")
        | select((.mergeStateStatus // "") == "CLEAN")
        | select(checks_failed(.) | not)
        | {number, title, updatedAt, url}
      ]
    ),
    next_actions: next_actions($repo; .; $prs[0]),
    recommendations: (
      [
        (if pcount("priority:critical") > 0 then
          "Keep a daily sync for priority:critical issues until count reaches 0."
        else empty end),
        (if (stale_issues($stale_days) | length) > 0 then
          "Review stale open issues and re-prioritize, close, or move to a milestone."
        else empty end),
        (if (($prs[0] | map(select(.state == "OPEN" and checks_failed(.))) | length) > 0) then
          "Prioritize fixing open PRs with failing checks before opening new implementation work."
        else empty end),
        (if (($prs[0] | map(select(.state == "OPEN" and (.reviewDecision // "") == "CHANGES_REQUESTED")) | length) > 0) then
          "Resolve CHANGES_REQUESTED PRs to reduce review queue latency."
        else empty end),
        (if $updated_since_last > 0 then
          "There are updated issues since last sync; refresh controller planning context."
        else
          "No issue updates since last sync; keep current controller plan."
        end)
      ]
    )
  }' "$WORK_ISSUES_JSON" > "$REPORT_JSON"

jq -n \
  --arg repo "$REPO" \
  --arg fetched_at "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" \
  --arg last_max_updated "$MAX_UPDATED" \
  --arg source_of_truth "github" \
  --arg issue_state "$ISSUE_STATE" \
  --arg pr_state "$ISSUE_STATE" \
  --argjson issue_count "$(jq 'length' "$WORK_ISSUES_JSON")" \
  --argjson pr_count "$(jq 'length' "$WORK_PRS_JSON")" \
  '{
    repo: $repo,
    source_of_truth: $source_of_truth,
    issue_state: $issue_state,
    pr_state: $pr_state,
    fetched_at: $fetched_at,
    last_max_updated_at: $last_max_updated,
    issue_count: $issue_count,
    pr_count: $pr_count
  }' > "$SYNC_STATE"

{
  echo "# Project Pulse Report"
  echo
  echo "- Repo: $REPO"
  echo "- Source of truth: GitHub"
  echo "- Issue scope: $ISSUE_STATE"
  echo "- PR scope: $ISSUE_STATE"
  echo "- Fetched at (UTC): $(jq -r '.metadata.fetched_at' "$REPORT_JSON")"
  echo "- Updated since last sync: $(jq -r '.metadata.updated_since_last' "$REPORT_JSON")"
  echo
  echo "## Issue Totals"
  echo "- Issues: $(jq -r '.totals.issues' "$REPORT_JSON")"
  echo "- Open: $(jq -r '.totals.open' "$REPORT_JSON")"
  echo "- Closed: $(jq -r '.totals.closed' "$REPORT_JSON")"
  echo "- priority:critical: $(jq -r '.totals.priority_critical' "$REPORT_JSON")"
  echo "- priority:high: $(jq -r '.totals.priority_high' "$REPORT_JSON")"
  echo "- priority:medium: $(jq -r '.totals.priority_medium' "$REPORT_JSON")"
  echo "- priority:low: $(jq -r '.totals.priority_low' "$REPORT_JSON")"
  echo
  echo "## PR Totals"
  echo "- PRs: $(jq -r '.pr_totals.prs' "$REPORT_JSON")"
  echo "- Open: $(jq -r '.pr_totals.open' "$REPORT_JSON")"
  echo "- Merged: $(jq -r '.pr_totals.merged' "$REPORT_JSON")"
  echo "- Closed: $(jq -r '.pr_totals.closed' "$REPORT_JSON")"
  echo "- Drafts: $(jq -r '.pr_totals.drafts' "$REPORT_JSON")"
  echo "- Failing checks: $(jq -r '.pr_totals.failing_checks' "$REPORT_JSON")"
  echo "- Changes requested: $(jq -r '.pr_totals.changes_requested' "$REPORT_JSON")"
  echo
  echo "## Stale Open Issues"
  STALE_ISSUES_COUNT="$(jq -r '.stale_issues | length' "$REPORT_JSON")"
  if [[ "$STALE_ISSUES_COUNT" -eq 0 ]]; then
    echo "- None"
  else
    jq -r '.stale_issues[] | "- #\(.number) [\(.priority)] age=\(.age_days)d \(.title)"' "$REPORT_JSON"
  fi
  echo
  echo "## Stalled PRs"
  STALLED_PRS_COUNT="$(jq -r '.stalled_prs | length' "$REPORT_JSON")"
  if [[ "$STALLED_PRS_COUNT" -eq 0 ]]; then
    echo "- None"
  else
    jq -r '.stalled_prs[] | "- #\(.number) [\(.merge_state)] age=\(.age_days)d \(.title)"' "$REPORT_JSON"
  fi
  echo
  echo "## Failing PRs"
  FAILING_PRS_COUNT="$(jq -r '.failing_prs | length' "$REPORT_JSON")"
  if [[ "$FAILING_PRS_COUNT" -eq 0 ]]; then
    echo "- None"
  else
    jq -r '.failing_prs[] | "- #\(.number) [\(.merge_state)] \(.title)"' "$REPORT_JSON"
  fi
  echo
  echo "## Ready To Merge Candidates"
  READY_PRS_COUNT="$(jq -r '.ready_to_merge_candidates | length' "$REPORT_JSON")"
  if [[ "$READY_PRS_COUNT" -eq 0 ]]; then
    echo "- None"
  else
    jq -r '.ready_to_merge_candidates[] | "- #\(.number) \(.title)"' "$REPORT_JSON"
  fi
  echo
  echo "## Controller Next Actions"
  jq -r '.next_actions[] | "- [\(.priority)] \(.action) \(.target): \(.reason)"' "$REPORT_JSON"
  echo
  echo "## Recommendations"
  jq -r '.recommendations[] | "- " + .' "$REPORT_JSON"
} > "$REPORT_MD"

echo "project-pulse completed"
if [[ "$SPLIT_ISSUES" == "true" ]]; then
  echo "issues dir:  $ISSUES_DIR"
  echo "issues idx:  $ISSUES_INDEX_JSON"
fi
if [[ -f "$ISSUES_JSON" ]]; then
  echo "issues:      $ISSUES_JSON"
fi
if [[ "$SPLIT_PRS" == "true" ]]; then
  echo "prs dir:     $PRS_DIR"
  echo "prs idx:     $PRS_INDEX_JSON"
fi
if [[ -f "$PRS_JSON" ]]; then
  echo "prs:         $PRS_JSON"
fi
echo "report json: $REPORT_JSON"
echo "report md:   $REPORT_MD"
echo "sync state:  $SYNC_STATE"

rm -f "$WORK_ISSUES_JSON" "$WORK_PRS_JSON"
