# Holon Android

This directory contains the native Android client implementation.

`:sdk` is a Kotlin/JVM library that keeps generated wire models separate from
stable client-facing domain models. `:app` is a Compose application with native
session login, recent conversations, Agent browsing, offline caches, a durable
prompt outbox, attachments, and brief/artifact viewing. Official signed APK/AAB
builds are described in [RELEASING.md](./RELEASING.md). Store listing, privacy
declarations and review access remain tracked in
[#3229](https://github.com/holon-run/holon/issues/3229).

Saved networks can be deleted from Settings or the sign-in screen after
confirmation. Deletion removes only that network's on-device configuration,
credentials, caches, drafts, unsent messages/attachments, and diagnostic logs;
the remote host and already submitted work are unaffected. Deleting the current
network disconnects and returns to sign-in without automatically connecting to
another saved network. Other networks and their local data are retained.

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

Task results appear inside the receiving turn's collapsed process. Task status
and a short failure/interruption reason remain visible before expansion; genuine
model responses remain in the main conversation. Source actions open the task
output reader on demand, including the original reply for reference-only results.

Pending background messages are grouped into a collapsed count card rather than
long paragraphs in the conversation. Expand the card for two-line previews,
then tap a message to read and copy the preview supplied by the runtime. Pending
operator messages remain separate conversation bubbles.

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

Client boundaries and shared iOS conformance inputs are described in
[the boundary decision](../../docs/implementation-decisions/android-client-boundaries.md).
The app keeps feature rendering/actions, active session binding, bounded
foreground synchronization, durable send state, and work/file reads separate.
Native organization sign-in now requires an updated daemon and an S256-bound
callback ticket; local-token login and confirmed HTTP remain compatible.

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

Run ordinary tests separately from screenshot verification:

```sh
./gradlew :sdk:test :app:testDebugUnitTest
./gradlew :app:verifyPaparazziDebug :app:lintRelease
./gradlew :app:connectedDebugAndroidTest
```

The instrumented suite is for isolated test devices/emulators: it creates test
sessions, modifies the test app's local preferences/database and verifies
migration, independent network scopes, system sharing and Activity/draft
restoration against a local fixture. Do not run it against a user's production
app data.

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
