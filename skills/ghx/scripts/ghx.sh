#!/bin/bash
# ghx.sh - Unified command entrypoint for ghx skill

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

usage() {
  cat <<USAGE
Usage:
  ghx.sh context collect <ref> [repo_hint]
  ghx.sh batch run --batch=<path> [--dry-run] [--from=N] [--pr=OWNER/REPO#NUM]
  ghx.sh review publish --pr=OWNER/REPO#NUM --body-file=<review.md|-> [--comments-file=review.json] [--max-inline=N] [--post-empty]
  ghx.sh pr create --repo=OWNER/REPO --title=... --body-file=<file|-> --head=... --base=...
  ghx.sh pr update --pr=OWNER/REPO#NUM [--title=...] [--body-file=<file|->] [--state=open|closed]
  ghx.sh pr comment --pr=OWNER/REPO#NUM --body-file=<summary.md|->
USAGE
}

if [[ $# -lt 1 ]]; then
  usage
  exit 2
fi

case "$1" in
  context)
    shift
    if [[ "${1:-}" != "collect" ]]; then
      usage
      exit 2
    fi
    shift
    exec "$SCRIPT_DIR/collect.sh" "$@"
    ;;
  batch)
    shift
    if [[ "${1:-}" != "run" ]]; then
      usage
      exit 2
    fi
    shift
    exec "$SCRIPT_DIR/publish.sh" "$@"
    ;;
  review)
    shift
    if [[ "${1:-}" != "publish" ]]; then
      usage
      exit 2
    fi
    shift
    exec "$SCRIPT_DIR/publish.sh" post-review "$@"
    ;;
  pr)
    shift
    sub="${1:-}"
    shift || true
    global_opts=()
    sub_args=()
    for arg in "$@"; do
      case "$arg" in
        --pr=*|--repo=*|--dry-run|--from=*)
          global_opts+=("$arg")
          ;;
        *)
          sub_args+=("$arg")
          ;;
      esac
    done
    case "$sub" in
      create)
        exec "$SCRIPT_DIR/publish.sh" "${global_opts[@]}" create-pr "${sub_args[@]}"
        ;;
      update)
        exec "$SCRIPT_DIR/publish.sh" "${global_opts[@]}" update-pr "${sub_args[@]}"
        ;;
      comment)
        exec "$SCRIPT_DIR/publish.sh" "${global_opts[@]}" comment "${sub_args[@]}"
        ;;
      *)
        usage
        exit 2
        ;;
    esac
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage
    exit 2
    ;;
esac
