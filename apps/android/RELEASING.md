# Android signing and releases

## Distribution and versions

Android ships with Holon's existing `vMAJOR.MINOR.PATCH` releases. The release
workflow builds, verifies, and attaches `holon-android-vX.Y.Z.apk`, `.aab`,
`.json` build metadata and checksums to the same GitHub Release as the runtime
and macOS app. GitHub Release is the durable download location; Actions artifacts
are temporary build/test outputs. AABs are for store submission, not installation.

Google Play uploads are a separate, manual
[internal-testing workflow](./PLAY_PUBLISHING.md); GitHub releases do not
automatically publish to Play.

`versionName` follows the root Cargo version. `versionCode` is
`major * 1_000_000 + minor * 1000 + patch`; minor/patch must be below 1000,
major at most 2099, and the result positive. Stable versions only: prereleases
are rejected until an explicit ordering is designed. Never replace an already
published APK with different code under the same version: ship the next Holon
patch release. Android-only releases can be introduced later with their own
documented monotonic version allocation, without changing the signing key.

The manual **Android signed release** workflow, dispatched on `main`, builds
the current Cargo version (or a matching explicit version) into Actions
artifacts without creating a release or bypassing the runtime E2E gate. New
workflow files must be merged before GitHub allows this dispatch.

## Signing identity and CI credentials

Package: `run.holon.android`. The official key is RSA 4096 / SHA256withRSA,
valid September 2026–September 2076. The public certificate SHA-256 fingerprint
is pinned in `signing/release-certificate.sha256`. This file is **not a key**.
The packaging script refuses a different keystore or output certificate,
unsigned/debuggable APKs, and incorrect package/version metadata.

Repository Actions secrets:

- `ANDROID_SIGNING_KEYSTORE_BASE64`: base64-encoded JKS. Base64 is not encryption.
- `ANDROID_SIGNING_STORE_PASSWORD`: keystore password.
- `ANDROID_SIGNING_KEY_ALIAS`: key alias (`holon-release`).
- `ANDROID_SIGNING_KEY_PASSWORD`: private key password.

Only trusted release tags and manual `main` builds use these secrets. Ordinary
pull-request CI uses no signing secrets and produces an unsigned release build.
The release workflow explicitly passes only these secrets to the reusable
Android job. The keystore is decoded under `RUNNER_TEMP`, removed on step exit,
and never uploaded. Signed builds disable Gradle configuration/build caches;
do not enable debug logging or upload signing configuration, Gradle state, or
secret files. Workflow and packaging-script changes require careful review:
maintainers able to change trusted release code can access signing credentials.

Back up the JKS, alias and both passwords together in controlled encrypted
storage, with an independent recovery copy. GitHub secrets are not a backup:
their values cannot be read back. Losing the standalone app signing key loses
the ability to issue normal updates. Never commit keys/passwords or regenerate
the official key to fix a build failure. Verify its public fingerprint when
restoring it on another machine.

## Local signed build

Configure `JAVA_HOME` (JDK 21), `ANDROID_HOME` and build-tools 36.0.0. Supply the
following environment variables from a secure source, not Gradle properties
checked into the repository:

```text
HOLON_ANDROID_KEYSTORE_PATH
HOLON_ANDROID_STORE_PASSWORD
HOLON_ANDROID_KEY_ALIAS
HOLON_ANDROID_KEY_PASSWORD
```

```sh
bash scripts/package-android-release.sh vX.Y.Z
```

This runs SDK/release unit tests, release lint, APK/AAB builds, signature and
manifest verification, then writes `dist/android/` assets with the source commit
and public certificate identity. It requires the requested version to match
Cargo.toml. Without signing variables, normal local `assembleRelease` remains
unsigned and must not be published; a partially configured signing environment
fails rather than silently falling back. Debug builds keep Android's local
debug keystore and never use the official key.

## Existing installs and stores

The official key differs from the previous local debug key. Existing debug
installs cannot be updated in place with the official APK. Before a one-time
reinstall, finish pending sends and retain any needed drafts/exports. Uninstall
removes local data and sessions; host-side Agents/results remain on the daemon.
We do not silently uninstall users' apps or claim a debug-to-release data migration.
Subsequent official releases with a higher versionCode update normally.

A release key does not by itself make the app store-ready. Google Play requires
Play App Signing for new apps. To preserve GitHub/Play cross-channel update
compatibility, supply this existing **app signing key** when enrolling rather
than accepting a new Google-generated identity; then register a separate upload
key and adjust AAB signing in CI. Until enrollment, the generated AAB uses the
app signing key (also usable as the initial upload key). Do not confuse upload
key rotation with the identity used on devices. No automatic Play upload is
implemented here. Privacy, listing, reviewer access, release-device/upgrade
testing and other store prerequisites remain in #3229.

References: [Android signing](https://developer.android.com/studio/publish/app-signing),
[Android versioning](https://developer.android.com/studio/publish/versioning),
[GitHub Actions secrets](https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/use-secrets).
