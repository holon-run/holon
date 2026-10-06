# Holon iOS

Native SwiftUI remote client, targeting iOS 18+ on iPhone and iPad. This directory
currently contains the P0 foundation, **not a finished client**. Connection,
conversation, sending, work/files and system sharing are implemented in later
stages.

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

- `Sources`: SwiftUI app and native credential adapter. Keychain accounts must
  carry the full connection/identity scope; credentials never belong in Core Data.
- `Tests`: simulator Keychain isolation/deletion, Core Data rollback/store-reopen
  and ATS configuration probes. Native authentication probes cover S256,
  state/callback validation, scoped pending-proof reopen/expiry/deletion and
  browser session preparation/cancellation. The temporary database is deleted
  by the test.
- [`../../packages/client-sdk-swift`](../../packages/client-sdk-swift):
  UI-independent wire/client layers and tests reading the shared fixture files.
- English and Simplified Chinese resources are provided. Agent content must
  never be translated with the UI.

P0 leaves arbitrary cleartext loads disabled. General user-supplied HTTP support
requires an explicit per-connection confirmation policy before broadening ATS.
The native browser adapter prepares an ephemeral `ASWebAuthenticationSession`
with the caller's window anchor and the fixed iOS callback. Pending proof belongs
in Keychain; retain it through uncertain exchange outcomes and remove it after
cancellation or confirmed exchange. Its API base URL includes `/api/` and any
proxy prefix. The P0 browser probe is HTTPS-only and does not present login UI.
It is a platform feasibility probe, not the P2 login coordinator or a completed
organization login flow.

Simulator probes do **not** certify physical-device LAN permissions, interactive
OIDC browser login/callback delivery, background execution or App Group/share-extension signing. Those
remain explicit validation gates, not assumed capabilities.
