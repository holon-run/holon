# iOS client foundation

## Choice

Build a SwiftUI iOS 18+ remote client in `apps/ios`, backed by the UI-independent
Swift package in `packages/client-sdk-swift`. Reuse OpenAPI Generator 7.25.0
already pinned by the repository, generating only a named schema closure and
array aliases into `HolonWire`. Keep its small model support separate from
generated files; do not introduce the generated network client or its framework.

The Swift5 generator flattens referenced `anyOf` models into a struct requiring
every branch's fields. The generation entrypoint instead emits an untagged
associated-value enum for object-reference unions, decoding in schema order
and encoding only the selected payload. Branch models retain their required
fields; unsupported union shapes fail generation rather than silently relaxing
the schema. Shared roster fixtures cover both workspace metadata variants.

Decode typed transport models alongside an open JSON document in `HolonClient`.
The generator's unknown-enum fallback permits forward-compatible decoding but
does not preserve original strings or arbitrary object extensions by itself.
Domain adapters therefore retain the original JSON rather than normalizing it
through the typed model. Wire types remain Swift 5; the stable client uses Swift 6.

## Preserved boundary

No Rust runtime or scheduler runs on the phone. Generated models own no retry,
auth, storage or UI policy. Keychain holds scoped credentials; the initial
hosted platform tests establish isolation/removal and Core Data transaction
reopen/rollback before a production outbox schema is introduced.

The P0 app kept arbitrary ATS loads disabled. P2 supports user-selected HTTP
addresses, including unknown IP literals and reverse proxies, with an explicit
per-profile confirmation enforced before constructing a transport. Static ATS
domain exceptions cannot enumerate these targets, so the app uses
`NSAllowsArbitraryLoads` without `NSAllowsLocalNetworking` (the latter overrides
the former). This removes ATS's additional restrictions, not default server trust
evaluation: URLSession still validates HTTPS certificates and denies redirects.
Organization login remains HTTPS-only. Store review will require an exception
justification; signing and distribution are not authorized by this choice.
OIDC requires the separately planned allow-listed iOS
callback while preserving Android defaults; registering Android's scheme is not
an acceptable workaround.

The native platform probe keeps pending S256 proof in Keychain, scoped by the
exact API base URL and random app state. It validates a fixed iOS callback,
unique query keys, state, S256 method and ticket shape before exchanging anything.
The browser adapter uses an ephemeral `ASWebAuthenticationSession` anchored to
the caller's window; attempt IDs suppress callbacks from a cancelled session.
Preparation/cancellation is testable without presenting UI. Interactive browser
login and real provider callback delivery remain separate P2 acceptance gates.

P2 stores small, secret-free network profiles and the selected profile ID in
UserDefaults; Keychain stores session identity, credentials and pending proof
locators. Profiles do not require database transactions or join the future
conversation/outbox store. Session lookup binds profile ID, complete API base,
runtime, user and visibility scope; every connection bootstrap revalidates these
dimensions before exposing a new identity. No conversation cache is introduced
by this authentication slice.

A confirmed exchange is staged in Keychain by profile/API base until that
bootstrap completes, so a transient post-login read failure or restart cannot
discard a known session credential. A staged credential is not an identity and
cannot select cached content. Promotion publishes the complete scope and deletes
the staged record; logout/profile removal deletes both. Uncertain native exchange
failures preserve bounded proof until explicit cancellation or expiry.

Pairing issue/redeem routes are authentication setup and remain available while
the daemon awaits model/provider configuration. Ticket issuance still requires
trusted-local admission or an authenticated control token/session; bootstrap
allow-listing does not weaken that handler check. The isolated-daemon probe
issues over its trusted Unix socket, confirms anonymous TCP issuance is denied,
and exercises single-use native redemption and session revocation in this mode.

## Evidence and outstanding gates

The P0 test entrypoints cover shared handshake/roster/error/session fixtures,
future enums/open fields, SDK compilation and hosted simulator platform probes.
They do not prove real-daemon HTTP/SSE/cancellation, native browser callback,
physical-device LAN privacy, share-extension signing or release distribution.
Those gates remain tracked in the implementation plan and must be reported
separately; an engineering scaffold is not client feature completion.
