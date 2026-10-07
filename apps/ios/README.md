# Holon iOS

Native SwiftUI remote client, targeting iOS 18+ on iPhone and iPad. This directory
contains first-connection onboarding, camera QR scanning, connection profiles,
token/session login, offline invitation preview and confirmed redemption,
native organization login, and scoped Keychain recovery.
The Agents tab provides foreground conversation reading, history, briefs and
on-demand execution details, with identity-partitioned offline caching.
Its conversation editor includes persistent drafts, local attachment staging,
model selection, an explicit current-run stop and a durable sending queue.
The three tabs are Agents, Work and Settings. Files, sharing, connections and
diagnostics remain available through Settings and work-artifact previews.
Internal TestFlight delivery does not establish physical-device or App Store
acceptance: see [release preparation](RELEASE.md) for the remaining gates.

## Using the client

1. On first launch, use **Scan connection QR code**, paste a connection invitation,
   or choose manual connection. Generate an invitation on your computer using
   Holon's connection QR entry. For manual connection, supply the full daemon
   API base, for example `https://example.com/proxy/api`, then authenticate with
   a daemon-issued token or organization sign-in when the server offers OIDC.
   After identity is confirmed, the app opens Agents. An empty agent roster is
   not a login failure. Saved sessions are restored before choosing the screen;
   failed recovery offers explicit retry and connection actions.
   A phone's loopback address points to the phone, not your development computer.
   For LAN access, use a reachable host and allow Local Network access in iOS
   Settings; check the daemon listener, firewall and proxy prefix if unreachable.
2. Prefer HTTPS. Only enable the explicit HTTP option for a target you trust
   after accepting that credentials and content travel without encryption.
   This consent is not TLS verification bypass. Organization login requires
   HTTPS. Do not expose an unauthenticated daemon to make pairing easier.
3. To pair, scan or paste the invitation's `/login#pair=<ticket>` contents.
   Check the displayed destination and HTTP warning before confirming redemption.
   Preview is offline; expiry and single use are enforced by the daemon.
   An address-only QR code still requires authentication. Camera denial or an
   unavailable camera leaves paste and manual connection available; frames are
   not saved or uploaded. There is no invitation history.
   In **Settings → Connections**, add or switch saved hosts; cancelling an
   added connection preserves the original host. Advanced authentication fields
   remain in the connection manager rather than the first-launch screen.
4. Select an agent to read conversations and briefs or edit a draft. Queueing
   is explicit; inspect the queue before retrying an unknown result. Unknown
   outcomes are not proof of failure: use the same-request retry rather than
   composing a duplicate. Stopping an observed run is separate from deleting
   a local queued request and does not retract already accepted server work.
5. In the iOS share sheet choose Holon. The extension stages input locally;
   it does not send to a daemon. Open **Settings → Share inbox**, reload,
   inspect the preview and select the target agent before confirming queueing.
   Discard unwanted inbox entries. Missing App Group access fails closed.
   A queued item and its staged inbox source have independent lifecycles.
6. In **Settings → Diagnostics**, prepare and inspect the allowlisted report before
   using the system share action. Sending it to an agent requires a separate
   confirmation; it is not automatic telemetry. The report contains status
   enums, counts, schema/platform and creation time, not arbitrary logs,
   profile addresses, credentials or conversation bodies.

### Logout, revocation and local cleanup

Logout attempts local credential deletion first and remote revocation second.
If storage fails, resolve the storage error; if the daemon is unreachable,
remote revocation is not proven. Revoke the session on the daemon through its
trusted administration path when necessary. Swipe-delete a profile to remove
its configuration; deleting configuration is not a server-work cancellation.

For each relevant authenticated agent scope, erase draft text and remove its
attachments in the editor; delete removable entries in the queue. Active sends
cannot be deleted there. Discard shared inbox entries separately. Switching
identity hides old scoped content but is not a general disk-wipe command.
Read-cache authorization failures invalidate the affected partition; there is
no user-facing purge-all cache button. Removing the app (not offloading it)
removes its ordinary sandbox, but is not a guarantee about Keychain, shared
containers, backups, exported reports or daemon-side data. Log out first.
File cleanup can fail and attachment copies can remain; no secure erasure or
historical orphan sweep is promised.

## Local verification

Use Xcode 16+ with an available iOS 18+ simulator, Swift 6, Node and the Java
runtime required by the repository's pinned OpenAPI Generator.

From the repository root:

```sh
make ios-ci
```

`ios-app-test` builds the checked-in project and runs hosted XCTest platform
probes. Set `IOS_SIMULATOR_ID` to select a device; otherwise the script selects
an available iPhone. Build output remains in ignored `apps/ios/DerivedData`.
The simulator uses local ad-hoc signing for its app-scoped Keychain entitlement;
it does not require an Apple distribution identity or upload.

Open `Holon.xcodeproj` and select the shared `Holon` scheme to run the app.
No development team, production credentials or signing profile is committed.
`run.holon.ios` and the configurable `HOLON_APP_GROUP` development placeholder
`group.run.holon.ios` are not confirmed App Store or registered App Group identifiers.
The app and `HolonShare` extension declare the same group, but only the app
declares its app-scoped Keychain group; credentials are never shared with the extension.
Configure a registered group and matching development profiles before physical-device
sharing validation. An unavailable shared container disables import rather than
falling back to a private path.

`ios-ci` also runs the full UI gate. Set `IOS_SIMULATOR_ID` to a dedicated fresh
simulator: removing the app does not guarantee removal of its saved Keychain
session. UI verification covers first connection, exactly three tabs, files,
diagnostics and accessibility. The diagnostic font-size test changes and reads
back the real simulator system preference without restarting Holon; it does not
test navigation in Apple's Settings app. Camera recognition and permission
lifecycles still require an iPhone or iPad.

## Boundaries

- `Sources`: connection coordinator, SwiftUI login/settings and native credential
  adapters. Small secret-free profiles live in UserDefaults; Keychain holds scoped
  sessions, staged exchange results and pending proof. Credentials never belong
  in preferences, Core Data or logs. A staged credential is not a cache identity.
- `Tests`: simulator Keychain isolation/deletion, Core Data rollback/store-reopen
  and ATS configuration probes. Native authentication probes cover S256,
  state/callback validation, scoped pending-proof reopen/expiry/deletion and
  browser session preparation/cancellation. The temporary database is deleted
  by the test.
- [`../../packages/client-sdk-swift`](../../packages/client-sdk-swift):
  UI-independent wire/client layers and tests reading the shared fixture files.
- English and Simplified Chinese resources are provided. Agent content must
  never be translated with the UI.
- Reading has one roster stream and at most one selected conversation stream.
  Scene inactivity cancels foreground work; returning rebuilds authoritative
  snapshots. Historical pagination does not advance the live cursor. Identity
  changes synchronously revoke the old view before another transport is bound.
  Cached content is read-only, contains no credentials, and is accessible only
  after network/runtime/user/visibility identity has been confirmed.
- Briefs are primary content, operator input is preserved, and execution details
  remain expandable rather than becoming another summary. Read-cursor failures
  have their own status; only server confirmation counts as a successful read.
  The manual read action uses an expanded, loaded brief visible in the viewport
  and confirms that all earlier results, including unopened history, are covered
  by the server's cumulative cursor. Agent/generation changes and hiding or
  collapsing a brief revoke that confirmation, even if the brief is shown again.
  The 80-entry brief cache evicts least-recently-used hidden content, not visible
  briefs; capacity is not a lifetime loading quota.
  List previews use bounded reads; unknown unread state is never
  displayed as zero.
- Sending uses a separate authenticated client and a Core Data store in
  Application Support. Drafts, managed attachments and queue entries are scoped
  by API/network/runtime/user/visibility and Agent; identity changes synchronously
  hide the old scope. A storage failure disables sending without disabling reading.
  Picker imports bind the original scope and draft generation; late results cannot
  enter a different identity, a revisited Agent or an already-enqueued draft.
  Enqueue saves the immutable request ID and submission before any prompt POST.
  Interrupted or response-lost submissions are unknown, not accepted: retry is
  explicit and preserves the original ID and prepared payload. A last-request
  offline status does not disable explicit retry with an active foreground
  identity; it never causes automatic resend. Cancelling a not-yet-started send
  leaves the durable entry queued and operable.
- Attachments are size-checked local copies. The current control prompt API
  accepts inline base64 attachments, not a separate upload/reference endpoint.
  A successful save removes retired copies only after their last draft/queue
  reference across all scopes disappears. Cleanup failures may leave copies;
  failed saves preserve references, and no historical orphan scan is promised.
  Model selection is a separate control request, not an atomic part of prompt
  acceptance. A stop targets an observed run ID and is distinct from cancelling
  a phone request. Neither background execution nor automatic unknown-outcome
  retries are promised.

Profiles require explicit HTTP confirmation, including loopback. HTTP itself
does not encrypt credentials or traffic. Because targets are arbitrary user
addresses, ATS permits arbitrary loads; SDK/profile validation enforces the
per-target confirmation and HTTPS keeps default certificate trust evaluation.
TLS trust bypasses and redirects are not supported. Store review will require
an ATS exception justification, not provided by simulator signing.
The native browser adapter prepares an ephemeral `ASWebAuthenticationSession`
with the caller's window anchor and the fixed iOS callback. Pending proof belongs
in Keychain; retain it through uncertain exchange outcomes and remove it after
cancellation or confirmed exchange. Its API base URL includes `/api/` and any
proxy prefix. Organization sign-in is HTTPS-only and available when the public
`auth/method` endpoint reports OIDC. The coordinator saves proof before starting
the browser, checks fixed scheme/state/S256/ticket, fences duplicate and stale
callbacks, and bounds login attempts. Browser-induced scene inactivity is not
cancellation; returning to the foreground rechecks proof expiry.

Use a full API base such as `https://example.com/proxy/api`. Import QR contents
by pasting the existing `/login#pair=<ticket>` URL; previewing makes no network
request. Confirm the displayed target and HTTP risk before redeeming. A proxy
prefix is preserved. The current QR format carries no expiry; the daemon enforces
expiry and single use. This stage does not request camera access or keep QR history.
Logout first deletes local credentials, then attempts remote revocation.

Simulator probes do **not** certify physical-device LAN permissions, interactive
OIDC browser login/callback delivery, background execution or App Group/share-extension signing. Those
remain explicit validation gates, not assumed capabilities.

## P5 capability alignment

| Android capability | iOS implementation and deliberate boundary |
| --- | --- |
| Work items and active tasks | Separate read-only lists and native details; no task creation, input or stop controls |
| Plans, result briefs and task output | Server plan metadata, linked brief navigation and explicitly truncated output; machine paths are not turned into locators |
| Workspace directories and filtering | Server workspace/root identity, hidden-file and name filters; removed worktrees and denied references remain errors |
| Images, Markdown, code and artifacts | Bounded native image/plain-text preview and explicit copy export/share; Markdown/code are readable source, not executable web content |
| Diagnostics | Local enum/count allowlist only; export or confirm sending to the current Agent, without raw logs or identity/payload fields |
| System sharing | A credential-free extension previews and stages text/links/files; the host confirms a target and queues an immutable request without replacing the editor draft or automatically sending |
| Android Direct Share/background services | No platform-for-platform copy, background resident SSE, automatic outbox retry or push promise |

Work and files use separate authenticated clients and lifecycle generations.
Tab/deep navigation does not recreate reading or sending coordinators. Identity
changes revoke old content and confirmations synchronously. Shared inputs have
the same canonical text and limits as the sending queue: 64 KiB UTF-8 text including
links/separators, at most 10 attachments and 20 MiB total attachment bytes.
The staged UUID is preserved on durable enqueue, and the shared copy is consumed
only after enqueue succeeds. Rejection leaves the original import recoverable.

Hosted tests exercise staging cancellation, unreadable/oversized inputs, store
reopen, identity revocation and independent enqueue. These are not share-sheet or
physical-device entitlement tests. The real-daemon probe covers work/task list
reads and explicit missing-record/file errors; successful populated work detail,
task output and signed device sharing remain separate verification gates.
