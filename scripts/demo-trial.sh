#!/usr/bin/env bash
# Demo canary upgrade and verification for the daily beta train.
#
# The demo node (https://demo.holon.run) is a Railway service built from this
# repository's Dockerfile. It is the beta canary: the release orchestrator
# upgrades it to the daily beta, verifies the rollout plus a live agent
# round-trip, lets the beta run for the 24h trial window, and only then
# promotes the beta to stable.
#
# Commands:
#   discover                       Print the Railway project/environment/service
#                                  that serves DEMO_DOMAIN as JSON.
#   upgrade --image <ref>          Point the demo service at <ref> and redeploy,
#                                  then wait for the new deployment to succeed.
#   verify [--expect-version <v>]  Public health + control status + agent
#                                  round-trip against the running demo daemon.
#   run --image <ref> [...]        upgrade followed by verify (daily timer path).
#
# Environment:
#   RAILWAY_TOKEN             Railway token for the workspace owning the demo
#                             service (required by discover/upgrade).
#   RAILWAY_API_URL           default https://backboard.railway.com/graphql/v2
#   DEMO_DOMAIN               default demo.holon.run
#   DEMO_CONTROL_TOKEN        Holon control token of the demo daemon (verify).
#   DEMO_AGENT_ID             agent used for the round-trip probe (verify).
#   DEMO_PROBE_TIMEOUT_SECS   round-trip budget, default 300.
#   DEMO_UPGRADE_TIMEOUT_SECS deployment budget, default 900.
#   HOLON_HEARTBEAT_ISSUE     optional GitHub issue for heartbeat comments.
#   HOLON_REPO                default holon-run/holon
#
# Exit codes: 0 success, 1 operational failure, 2 usage error.
set -euo pipefail

REPO="${HOLON_REPO:-holon-run/holon}"
RAILWAY_API_URL="${RAILWAY_API_URL:-https://backboard.railway.com/graphql/v2}"
DEMO_DOMAIN="${DEMO_DOMAIN:-demo.holon.run}"
PROBE_TIMEOUT="${DEMO_PROBE_TIMEOUT_SECS:-300}"
UPGRADE_TIMEOUT="${DEMO_UPGRADE_TIMEOUT_SECS:-900}"
ISSUE="${HOLON_HEARTBEAT_ISSUE:-}"

log() { printf '[demo-trial] %s\n' "$*"; }
die() { printf '[demo-trial] ERROR: %s\n' "$*" >&2; exit 1; }

usage() {
  sed -n '2,31p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
}

heartbeat() {
  [ -n "$ISSUE" ] || return 0
  command -v gh >/dev/null 2>&1 || return 0
  gh issue comment "$ISSUE" --repo "$REPO" --body "demo-trial: $*" >/dev/null 2>&1 || true
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

gql() { # <query> [variables-json]
  local query="$1" variables="${2:-}" payload response
  [ -n "$variables" ] || variables='{}'
  payload="$(jq -cn --arg q "$query" --argjson v "$variables" '{query: $q, variables: $v}')"
  response="$(curl -fsS -X POST "$RAILWAY_API_URL" \
    -H "Authorization: Bearer ${RAILWAY_TOKEN}" \
    -H 'Content-Type: application/json' \
    -d "$payload")" || die "Railway API request to ${RAILWAY_API_URL} failed"
  jq -e '.errors == null or (.errors | length) == 0' >/dev/null <<<"$response" ||
    die "Railway API returned errors: $(jq -c '.errors' <<<"$response")"
  printf '%s\n' "$response"
}

require_railway() {
  need_cmd curl
  need_cmd jq
  [ -n "${RAILWAY_TOKEN:-}" ] || die "RAILWAY_TOKEN is required to discover or upgrade the demo service"
}

DISCOVER_QUERY='query { me { workspaces { id name projects { edges { node { id name environments { edges { node { id name } } } } } } } } }'
DOMAINS_QUERY='query($projectId: String!) { domains(projectId: $projectId) { serviceDomains { domain environmentId serviceId } } }'

discover() {
  require_railway
  local tree project_ids project_id domains hit=""
  tree="$(gql "$DISCOVER_QUERY")"
  project_ids="$(jq -r '.data.me.workspaces[].projects.edges[].node.id' <<<"$tree")"
  [ -n "$project_ids" ] || die "Railway token sees no projects; check the token's workspace"
  for project_id in $project_ids; do
    domains="$(gql "$DOMAINS_QUERY" "$(jq -cn --arg id "$project_id" '{projectId: $id}')")"
    hit="$(jq -c --arg d "$DEMO_DOMAIN" --arg p "$project_id" \
      '.data.domains.serviceDomains[]? | select(.domain == $d)
       | {projectId: $p, environmentId: .environmentId, serviceId: .serviceId, domain: .domain}' <<<"$domains")"
    if [ -n "$hit" ]; then
      printf '%s\n' "$hit"
      return 0
    fi
  done
  die "no Railway service domain matches ${DEMO_DOMAIN}; the token must belong to the workspace that owns the demo service"
}

service_state() { # <environmentId> <serviceId>
  gql 'query($environmentId: String!, $serviceId: String!) {
         serviceInstance(environmentId: $environmentId, serviceId: $serviceId) {
           source { image }
           latestDeployment { id status createdAt url }
         }
       }' "$(jq -cn --arg e "$1" --arg s "$2" '{environmentId: $e, serviceId: $s}')"
}

wait_for_deployment() { # <environmentId> <serviceId> <image>
  local env_id="$1" service_id="$2" image="$3" deadline status
  deadline=$(( $(date -u +%s) + UPGRADE_TIMEOUT ))
  while [ "$(date -u +%s)" -lt "$deadline" ]; do
    status="$(service_state "$env_id" "$service_id")"
    local deploy_status current_image
    deploy_status="$(jq -r '.data.serviceInstance.latestDeployment.status // "none"' <<<"$status")"
    current_image="$(jq -r '.data.serviceInstance.source.image // "none"' <<<"$status")"
    log "deployment ${deploy_status}; source image ${current_image}"
    case "$deploy_status" in
      SUCCESS)
        [ "$current_image" = "$image" ] ||
          die "deployment succeeded but the source image is ${current_image}, expected ${image}"
        printf '%s\n' "$status"
        return 0
        ;;
      FAILED|CRASHED|REMOVED)
        heartbeat "FAILED: demo deployment of ${image} ended ${deploy_status}"
        die "demo deployment of ${image} ended ${deploy_status}"
        ;;
    esac
    sleep 15
  done
  heartbeat "FAILED: demo deployment of ${image} did not finish within ${UPGRADE_TIMEOUT}s"
  die "demo deployment of ${image} did not finish within ${UPGRADE_TIMEOUT}s"
}

upgrade() { # <image>
  local image="$1" target env_id service_id before
  case "$image" in
    ''|*[!A-Za-z0-9._:/@-]*) die "invalid image reference: ${image}" ;;
  esac
  target="$(discover)"
  env_id="$(jq -r '.environmentId' <<<"$target")"
  service_id="$(jq -r '.serviceId' <<<"$target")"
  before="$(service_state "$env_id" "$service_id")"
  log "demo service: project $(jq -r '.projectId' <<<"$target"), environment ${env_id}, service ${service_id}"
  log "current source image: $(jq -r '.data.serviceInstance.source.image // "none"' <<<"$before")"

  jq -e '.data.serviceInstanceUpdate == true' >/dev/null <<<"$(
    gql 'mutation($environmentId: String!, $serviceId: String!, $image: String!) {
           serviceInstanceUpdate(environmentId: $environmentId, serviceId: $serviceId,
             input: { source: { image: $image } })
         }' "$(jq -cn --arg e "$env_id" --arg s "$service_id" --arg i "$image" \
               '{environmentId: $e, serviceId: $s, image: $i}')")" ||
    die "could not point the demo service at ${image}"

  jq -e '.data.serviceInstanceRedeploy == true' >/dev/null <<<"$(
    gql 'mutation($environmentId: String!, $serviceId: String!) {
           serviceInstanceRedeploy(environmentId: $environmentId, serviceId: $serviceId)
         }' "$(jq -cn --arg e "$env_id" --arg s "$service_id" \
               '{environmentId: $e, serviceId: $s}')")" ||
    die "could not redeploy the demo service"

  log "redeploy requested for ${image}"
  wait_for_deployment "$env_id" "$service_id" "$image" >/dev/null
  log "demo service is running ${image}"
  heartbeat "demo upgraded to ${image} (deployment succeeded)"
}

control_get() { # <path>
  curl -fsS "https://${DEMO_DOMAIN}${1}" -H "Authorization: Bearer ${DEMO_CONTROL_TOKEN}"
}

verify_agent_roundtrip() { # <product-version>
  local probe deadline briefs
  probe="HOLON-BETA-PROBE-$(date -u +%Y%m%dT%H%M%SZ)-$RANDOM"
  curl -fsS -X POST "https://${DEMO_DOMAIN}/api/control/agents/${DEMO_AGENT_ID}/prompt" \
    -H "Authorization: Bearer ${DEMO_CONTROL_TOKEN}" \
    -H 'Content-Type: application/json' \
    -d "$(jq -cn --arg t "Reply with exactly: ${probe}" '{text: $t}')" >/dev/null ||
    die "could not enqueue the probe prompt on agent ${DEMO_AGENT_ID}"
  log "probe ${probe} enqueued on agent ${DEMO_AGENT_ID}; waiting up to ${PROBE_TIMEOUT}s for the reply"
  deadline=$(( $(date -u +%s) + PROBE_TIMEOUT ))
  while [ "$(date -u +%s)" -lt "$deadline" ]; do
    briefs="$(control_get "/api/agents/${DEMO_AGENT_ID}/briefs" 2>/dev/null || true)"
    if grep -q "$probe" <<<"$briefs"; then
      log "agent round-trip confirmed: ${probe}"
      return 0
    fi
    sleep 10
  done
  heartbeat "FAILED: agent round-trip on the demo (${1:-unknown}) timed out after ${PROBE_TIMEOUT}s"
  die "agent round-trip on the demo timed out after ${PROBE_TIMEOUT}s"
}

verify() { # [expect-version]
  local expect="${1:-}" status code product
  need_cmd curl
  need_cmd jq
  [ -n "${DEMO_CONTROL_TOKEN:-}" ] || die "DEMO_CONTROL_TOKEN is required to verify the demo daemon"
  [ -n "${DEMO_AGENT_ID:-}" ] || die "DEMO_AGENT_ID is required for the agent round-trip"

  code="$(curl -sS -o /dev/null -w '%{http_code}' "https://${DEMO_DOMAIN}/")" ||
    die "demo web GUI is unreachable"
  [ "$code" = "200" ] || die "demo web GUI answered ${code}, expected 200"
  log "web GUI healthy (200)"

  status="$(control_get /api/control/runtime/status)" || die "demo control status is unreachable"
  jq -e '[.. | objects | .healthy?] | any(. == true)' >/dev/null <<<"$status" ||
    die "demo control status reports unhealthy: ${status:0:300}"
  product="$(jq -r '[.. | objects | .product_version? // empty] | first // "unknown"' <<<"$status")"
  log "control status healthy; product_version ${product}"
  if [ -n "$expect" ] && ! grep -qF "$expect" <<<"$product"; then
    die "demo reports product_version ${product}, expected it to contain ${expect}"
  fi

  verify_agent_roundtrip "$product"
  heartbeat "PASS: demo healthy on ${product}$([ -n "$expect" ] && printf ' (expected %s)' "$expect"); agent round-trip ok"
}

SCENARIO="${1:-}"
[ $# -gt 0 ] && shift || true
IMAGE=""
EXPECT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --image) IMAGE="${2:-}"; shift 2 ;;
    --expect-version) EXPECT="${2:-}"; shift 2 ;;
    -h|--help) usage ;;
    *) die "unknown argument: $1" ;;
  esac
done

case "$SCENARIO" in
  discover) discover ;;
  upgrade)
    [ -n "$IMAGE" ] || die "upgrade needs --image <ref>"
    upgrade "$IMAGE"
    ;;
  verify) verify "$EXPECT" ;;
  run)
    [ -n "$IMAGE" ] || die "run needs --image <ref>"
    upgrade "$IMAGE"
    verify "$EXPECT"
    ;;
  ''|-h|--help) usage ;;
  *) die "unknown command: ${SCENARIO}" ;;
esac
