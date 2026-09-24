# Holon Android

This directory contains the native Android client implementation.

`:sdk` is a Kotlin/JVM library that keeps generated wire models separate from
stable client-facing domain models. `:app` is a Compose application with native
session login, recent conversations, Agent browsing, offline caches, a durable
prompt outbox, attachments, and brief/artifact viewing. Store signing and public
distribution configuration are intentionally out of scope.

The generated transport sources remain owned by
`packages/client-wire-kotlin`. Do not copy or edit those models in this
directory.

Run the SDK tests from the repository root:

```sh
make android-sdk-test
make android-sdk-integration-test
```

The checked-in Gradle Wrapper pins Gradle 8.14.3. A JDK 21 installation is the
only required local build prerequisite; no system Gradle installation is
needed. The integration target also builds and starts the repository's real
`holon` daemon for the read-only handshake and Agent roster contract.

Build the app with the Android SDK installed and `ANDROID_HOME` configured:

```sh
cd apps/android
./gradlew :app:assembleDebug :app:assembleRelease :sdk:test
```

The debug app defaults to `http://10.0.2.2:7878/api` on an Android emulator and
`http://127.0.0.1:7878/api` on a physical device. For a USB-connected device,
run `adb reverse tcp:7878 tcp:7878` before connecting. Only the debug build
allows loopback HTTP without confirmation. Both debug and release builds allow
an explicitly confirmed HTTP address for self-hosted LAN and encrypted-tunnel
deployments. The login screen warns that HTTP itself does not encrypt traffic.
Session login exchanges a local auth or one-time bootstrap token for a revocable
native session. Only the session is encrypted with Android Keystore; the input
token is not persisted.

`HolonHttpClient` accepts the API base URL. For a directly connected daemon,
use an address ending in `/api`; reverse proxies may supply another API prefix.
