# Holon Android

This directory contains the native Android client implementation.

`:sdk` is a Kotlin/JVM library that keeps generated wire models separate from
stable client-facing domain models. `:app` is a minimal Compose application for
the development connection flow; it does not provide production login, signing,
or distribution configuration.

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
permits cleartext traffic to these loopback development hosts. Its session
credential field accepts an already-exchanged session credential and stores it
using Android Keystore;
leaving it blank does not create a session. The release build does not expose
session injection or cleartext traffic and cannot log in yet.

`HolonHttpClient` accepts the API base URL. For a directly connected daemon,
use an address ending in `/api`; reverse proxies may supply another API prefix.
