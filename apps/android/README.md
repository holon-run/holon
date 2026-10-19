# Holon Android

This directory contains the native Android client implementation.

Markdown links to this runtime's hosted Apps and exact GUI routes open in an
authenticated in-app WebView; external sites require confirmation and open in
the system browser without the native session. File/artifact links still use
the existing resolver, and rendering a web link does not resolve a file or
request access.

The in-app browser binds the existing opaque session as a host-only, HttpOnly,
SameSite cookie (Secure on HTTPS), never a URL parameter or JavaScript bridge.
**Cookies are not isolated by port:** services on the same authentication
hostname must belong to one trusted boundary. Top-level navigation is still
restricted to the selected runtime's full origin; redirect interception is not
a port-level credential firewall. HTTP retains the existing explicit insecure
connection confirmation.

Each opening uses a fresh AndroidX WebKit Profile. Changing connections,
credentials or accounts retires the page; closing clears its cookies, web
storage and Service Workers without revoking the shared native session.
WebView providers must support both `MULTI_PROFILE` and
`DELETE_BROWSING_DATA`; unsupported providers fail closed, without a global
cookie fallback. Profile data may reach disk; names are never reused and
leftover profiles are deleted on cold start. This is client identity separation,
not a new permission sandbox for same-origin hosted Apps. See the
[browser-session decision](../../docs/implementation-decisions/android-runtime-browser-session.md).

After building the debug app and instrumentation APK, verify the native login,
authenticated App SDK context/request/events round trip and shared-session
logout against a disposable scripted daemon:

```sh
python3 scripts/android-browser-integration.py \
  --serial <dedicated-emulator-serial> --artifacts <new-private-directory>
```

Run this command from the repository root, **not on a production device**. It
installs the test app, uses loopback-only disposable credentials and removes its
adb reverse and private on-device fixture on exit. Real IdP/OIDC sign-in and
physical-device acceptance remain separate checks.

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

New connections start with an empty server address in both debug and release
builds. Enter your own host address or scan its connection QR code. **Next**
reads the server's public `/api/auth/method`: local-token hosts show token entry,
while OIDC hosts open the native organization sign-in flow in a browser. Failed
or unsupported discovery stays on the address step with an error, never a guessed
authentication fallback. Changing the address clears the authentication step
and any entered token; saved networks and confirmed one-time pairing remain available.

For local development, enter `http://10.0.2.2:7878` on an Android emulator or
`http://127.0.0.1:7878` on a USB-connected device after running
`adb reverse tcp:7878 tcp:7878`. Only the debug build
allows loopback HTTP without confirmation. Both debug and release builds allow
an explicitly confirmed HTTP address for self-hosted LAN and encrypted-tunnel
deployments. The login screen warns that HTTP itself does not encrypt traffic.
Session login exchanges a local auth or one-time bootstrap token for a revocable
native session. Only the session is encrypted with Android Keystore; the input
token is not persisted.

`HolonHttpClient` accepts the API base URL. For a directly connected daemon,
use an address ending in `/api`; reverse proxies may supply another API prefix.
