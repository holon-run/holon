#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_dir"
version="${1:?Usage: package-android-release.sh vMAJOR.MINOR.PATCH}"
version_data="$(bash scripts/android-release-version.sh "$version")"
version_name="$(printf '%s\n' "$version_data" | sed -n 's/^version_name=//p')"
version_code="$(printf '%s\n' "$version_data" | sed -n 's/^version_code=//p')"
crate_version="$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)"
[[ "$crate_version" == "$version_name" ]] || { echo 'Android release version must match Cargo.toml.' >&2; exit 1; }
for name in HOLON_ANDROID_KEYSTORE_PATH HOLON_ANDROID_STORE_PASSWORD HOLON_ANDROID_KEY_ALIAS HOLON_ANDROID_KEY_PASSWORD ANDROID_HOME JAVA_HOME; do
  [[ -n "${!name:-}" ]] || { echo "Required release environment variable missing: $name" >&2; exit 1; }
done
[[ -f "$HOLON_ANDROID_KEYSTORE_PATH" ]] || { echo 'Release keystore not found.' >&2; exit 1; }
expected_cert="$(tr -d '\r\n' < apps/android/signing/release-certificate.sha256)"
[[ "$expected_cert" =~ ^[a-f0-9]{64}$ ]] || { echo 'Invalid official certificate fingerprint.' >&2; exit 1; }
certificate_fingerprint() {
  openssl x509 -noout -fingerprint -sha256 | sed 's/.*=//' | tr -d ':' | tr '[:upper:]' '[:lower:]'
}
actual_cert="$("$JAVA_HOME/bin/keytool" -exportcert -rfc -keystore "$HOLON_ANDROID_KEYSTORE_PATH" \
  -alias "$HOLON_ANDROID_KEY_ALIAS" -storepass:env HOLON_ANDROID_STORE_PASSWORD | certificate_fingerprint)"
[[ "$actual_cert" == "$expected_cert" ]] || { echo 'Keystore is not the pinned official signing identity.' >&2; exit 1; }

export HOLON_ANDROID_VERSION_NAME="$version_name"
(
  cd apps/android
  # Signing configuration contains secrets: never serialize it in Gradle's
  # configuration cache or retain signed task output in a shared build cache.
  ./gradlew --no-daemon --no-configuration-cache --no-build-cache \
    :sdk:test :app:testReleaseUnitTest :app:lintRelease :app:assembleRelease :app:bundleRelease
)
apk=apps/android/app/build/outputs/apk/release/app-release.apk
aab=apps/android/app/build/outputs/bundle/release/app-release.aab
tools_dir="$ANDROID_HOME/build-tools/36.0.0"
apk_cert="$("$tools_dir/apksigner" verify --verbose --print-certs "$apk" | sed -n 's/^Signer #1 certificate SHA-256 digest: //p')"
[[ "$apk_cert" == "$expected_cert" ]] || { echo 'APK signature verification failed.' >&2; exit 1; }
badging="$("$tools_dir/aapt" dump badging "$apk")"
[[ "$badging" == *"package: name='run.holon.android' versionCode='$version_code' versionName='$version_name'"* ]] || { echo 'APK package/version verification failed.' >&2; exit 1; }
if [[ "$badging" == *application-debuggable* ]]; then echo 'Refusing to publish a debuggable APK.' >&2; exit 1; fi
aab_verification="$("$JAVA_HOME/bin/jarsigner" -J-Duser.language=en -verify "$aab")"
[[ "$aab_verification" == *'jar verified.'* ]] || { echo 'AAB integrity verification failed.' >&2; exit 1; }
aab_cert="$("$JAVA_HOME/bin/keytool" -printcert -rfc -jarfile "$aab" | certificate_fingerprint)"
[[ "$aab_cert" == "$expected_cert" ]] || { echo 'AAB signature verification failed.' >&2; exit 1; }

mkdir -p dist/android
asset="holon-android-v$version_name"
cp "$apk" "dist/android/$asset.apk"
cp "$aab" "dist/android/$asset.aab"
jq -n --arg version "$version_name" --argjson version_code "$version_code" \
  --arg commit "$(git rev-parse HEAD)" --arg certificate_sha256 "$expected_cert" \
  --arg apk "$asset.apk" --arg aab "$asset.aab" \
  '{version: $version, version_code: $version_code, commit: $commit,
    package: "run.holon.android", certificate_sha256: $certificate_sha256,
    min_sdk: 26, target_sdk: 36, apk: $apk, aab: $aab}' > "dist/android/$asset.json"
(
  cd dist/android
  shasum -a 256 "$asset.apk" "$asset.aab" "$asset.json" > "$asset.sha256"
)
echo "Verified signed Android artifacts: dist/android/$asset.{apk,aab,json,sha256}"
