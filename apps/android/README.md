# Holon Android

This directory contains the native Android client implementation.

`:sdk` is a Kotlin/JVM library that keeps generated wire models separate from
stable client-facing domain models. `:app` is a Compose application with native
session login, recent conversations, Agent browsing, offline caches, a durable
prompt outbox, attachments, and brief/artifact viewing. Official signed APK/AAB
builds are described in [RELEASING.md](./RELEASING.md). Store listing, privacy
declarations and review access remain tracked in
[#3229](https://github.com/holon-run/holon/issues/3229).

The app UI supports English and Simplified Chinese. It follows the device or
Android per-app language by default; the login and Settings screens also offer
an app-language override (System default, English, 简体中文). The override stays
on this device across sign-out. Agent messages, briefs, tool output, and file
contents are displayed as authored, without translation.

Agent rows show the newest operator input until that turn has a brief. Model
selection starts with models used by Agents on the current network, with the
full searchable catalog still available. The Work page separates active Tasks
(command and child-Agent progress, status and output preview) from WorkItems.
Task updates are foreground/event-driven; the refresh controls can also reload
status and output. Read-receipt transport failures retry without error banners
and remain available in the diagnostic trace.

The Settings connection diagnostics section can export a redacted trace or send
it directly to a selected Agent. Android's Share menu can also send text, links,
images, and files to Holon. The app previews the content and asks which Agent
should receive it before enqueueing a new message; an existing composer draft is
left alone. Recent Agents are published as Android Direct Share shortcuts when a
session is active. Android controls how many shortcuts appear and their ranking.
Shortcuts are refreshed for the current runtime and user, and removed on sign-out.

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
