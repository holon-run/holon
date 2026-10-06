# Holon iOS

Native SwiftUI remote client, targeting iOS 18+ on iPhone and iPad. This directory
contains connection profiles, token/session login, offline QR invitation preview
and confirmed redemption, native organization login, and scoped Keychain recovery.
It is **not a finished client**: conversation reading, reliable sending,
work/files and system sharing belong to later stages.

## Local verification

Use Xcode 16+ with an available iOS 18+ simulator, Swift 6, Node and the Java
runtime required by the repository's pinned OpenAPI Generator.

From the repository root:

```sh
make ios-wire-check
make ios-sdk-test
make ios-app-test
```

`ios-app-test` builds the checked-in project and runs hosted XCTest platform
probes. Set `IOS_SIMULATOR_ID` to select a device; otherwise the script selects
an available iPhone. Build output remains in ignored `apps/ios/DerivedData`.
The simulator uses local ad-hoc signing for its app-scoped Keychain entitlement;
it does not require an Apple distribution identity or upload.

Open `Holon.xcodeproj` and select the shared `Holon` scheme to run the app.
No development team, production credentials, signing profile or App Group is
committed. `run.holon.ios` is the local development Bundle ID, not a confirmed
App Store identifier.

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
