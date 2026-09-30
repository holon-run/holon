#!/usr/bin/env bash
set -euo pipefail
repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_dir"
unset HOLON_ANDROID_KEYSTORE_PATH HOLON_ANDROID_STORE_PASSWORD HOLON_ANDROID_KEY_ALIAS HOLON_ANDROID_KEY_PASSWORD
test_dir="$(mktemp -d)"
trap 'rm -f "$test_dir/test.jks" "$test_dir/output"; rmdir "$test_dir"' EXIT
expect_rejection() {
  local expected="$1"
  shift
  if bash scripts/package-android-release.sh "$@" > "$test_dir/output" 2>&1; then
    echo 'Packaging unexpectedly accepted invalid release inputs.' >&2
    exit 1
  fi
  grep -Fq "$expected" "$test_dir/output" || { echo "Expected rejection: $expected" >&2; exit 1; }
}
version="$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)"
expect_rejection 'version must match Cargo.toml' v2099.999.999
expect_rejection 'Required release environment variable missing: HOLON_ANDROID_KEYSTORE_PATH' "$version"

# An ephemeral test key, never the real signing identity or local debug key.
export HOLON_ANDROID_KEYSTORE_PATH="$test_dir/test.jks"
export HOLON_ANDROID_STORE_PASSWORD=ephemeral-test-password
export HOLON_ANDROID_KEY_PASSWORD="$HOLON_ANDROID_STORE_PASSWORD"
export HOLON_ANDROID_KEY_ALIAS=test-only
"${JAVA_HOME:?}/bin/keytool" -genkeypair -keystore "$HOLON_ANDROID_KEYSTORE_PATH" \
  -storetype JKS -alias "$HOLON_ANDROID_KEY_ALIAS" -keyalg RSA -keysize 2048 \
  -validity 1 -dname 'CN=Android Debug' -storepass:env HOLON_ANDROID_STORE_PASSWORD \
  -keypass:env HOLON_ANDROID_KEY_PASSWORD > "$test_dir/output" 2>&1
expect_rejection 'Keystore is not the pinned official signing identity' "$version"
echo 'Android packaging rejection tests passed.'
