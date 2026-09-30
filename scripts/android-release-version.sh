#!/usr/bin/env bash
set -euo pipefail

# Reserve three decimal places each for minor and patch. Reject prereleases and
# oversized components rather than inventing an ambiguous upgrade ordering.
input="${1:?Usage: android-release-version.sh vMAJOR.MINOR.PATCH}"
version="${input#v}"
if [[ ! "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo 'Expected a stable major.minor.patch Android release version.' >&2
  exit 1
fi
major="${BASH_REMATCH[1]}"
minor="${BASH_REMATCH[2]}"
patch="${BASH_REMATCH[3]}"
if (( ${#major} > 4 || ${#minor} > 3 || ${#patch} > 3 )); then
  echo 'Android version components exceed the versionCode allocation.' >&2
  exit 1
fi
code=$((major * 1000000 + minor * 1000 + patch))
if (( major > 2099 || code < 1 || code > 2100000000 )); then
  echo 'Android versionCode is outside the supported range.' >&2
  exit 1
fi
printf 'version_name=%s\nversion_code=%s\n' "$version" "$code"
