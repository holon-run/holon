#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 2 ]]; then
  echo "usage: $0 <version> <version-output>" >&2
  exit 2
fi

expected_version="$1"
actual="$2"
prefix="holon ${expected_version} ("

if [[ "$actual" != "$prefix"*")" ]]; then
  echo "unexpected version output: $actual" >&2
  exit 1
fi

commit_sha="${actual#"$prefix"}"
commit_sha="${commit_sha%")"}"
if [[ ! "$commit_sha" =~ ^[0-9a-f]{7,40}$ ]]; then
  echo "unexpected commit SHA in version output: $actual" >&2
  exit 1
fi
