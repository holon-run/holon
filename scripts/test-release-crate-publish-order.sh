#!/usr/bin/env bash
set -euo pipefail

workflow=".github/workflows/release.yml"
root_manifest="Cargo.toml"

mapfile -t local_crates < <(
  awk '
    /^\[dependencies\]/ { in_dependencies=1; next }
    /^\[/ { in_dependencies=0 }
    in_dependencies && /path = "crates\/[^"]+"/ && !/optional = true/ {
      match($0, /path = "crates\/([^"]+)"/, parts)
      print parts[1]
    }
  ' "$root_manifest"
)

mapfile -t published_crates < <(
  awk '
    /publish_if_needed [A-Za-z0-9_-]+/ {
      sub(/^.*publish_if_needed /, "")
      print $1
    }
  ' "$workflow"
)

for crate in "${local_crates[@]}"; do
  if ! printf '%s\n' "${published_crates[@]}" | grep -Fxq "$crate"; then
    echo "missing local dependency crate from release publish order: $crate" >&2
    exit 1
  fi
done

echo "release publish order covers all required root local dependency crates"
