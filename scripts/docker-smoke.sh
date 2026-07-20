#!/usr/bin/env bash
set -euo pipefail

image="${1:-holon:dev}"
container="holon-docker-smoke-$$"
volume="holon-docker-smoke-$$"
token="holon-docker-smoke-token-$$"

cleanup() {
  docker rm -f "$container" >/dev/null 2>&1 || true
  docker volume rm "$volume" >/dev/null 2>&1 || true
}
trap cleanup EXIT

expected_version="$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)"
actual_version="$(docker run --rm "$image" --version)"
if [ "$actual_version" != "holon $expected_version" ]; then
  echo "unexpected image version: $actual_version (expected holon $expected_version)" >&2
  exit 1
fi

docker volume create "$volume" >/dev/null
docker run --detach \
  --name "$container" \
  --env "HOLON_CONTROL_TOKEN=$token" \
  --env "HOLON_MODEL=openai/gpt-5.4" \
  --env "OPENAI_API_KEY=holon-docker-smoke-not-a-real-key" \
  --publish 127.0.0.1::7878 \
  --volume "$volume:/var/lib/holon" \
  "$image" >/dev/null

host_port="$(
  docker port "$container" 7878/tcp \
    | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' \
    | head -n 1
)"
if [ -z "$host_port" ]; then
  echo "failed to resolve the published Holon port" >&2
  docker logs "$container" >&2
  exit 1
fi

for _ in $(seq 1 60); do
  if curl --fail --silent \
    --header "Authorization: Bearer $token" \
    "http://127.0.0.1:$host_port/api/control/runtime/readiness" \
    >/dev/null; then
    echo "Docker smoke passed for $image on port $host_port."
    exit 0
  fi
  if [ "$(docker inspect --format '{{.State.Running}}' "$container")" != "true" ]; then
    echo "Holon container exited before becoming ready" >&2
    docker logs "$container" >&2
    exit 1
  fi
  sleep 1
done

echo "Holon did not become ready within 60 seconds" >&2
docker logs "$container" >&2
exit 1
