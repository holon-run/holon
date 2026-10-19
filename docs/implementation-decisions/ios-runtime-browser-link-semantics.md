# iOS runtime browser and Markdown link semantics

## Choice

The iOS app classifies Markdown web links with the same semantics as Web
(#3599) and Android (#3608): Holon App links (`/apps/...`), GUI routes, bare
app paths, and reference-style definitions resolve against the connected
runtime origin; non-runtime destinations stay external. Runtime destinations
open in an in-app WKWebView browser (#3603) instead of an external browser.
Every open creates a fresh non-persistent `WKWebsiteDataStore`; the session is
bound by setting the HttpOnly `holon_session` cookie through the
`WKHTTPCookieStore` before the first load, and the href is re-validated
(classify plus same-origin resolve) at open time even when it arrives through
the `holon-web:` marker path.

## Reason

- Binding the cookie through the WKHTTPCookieStore keeps the credential out of
  URLs, JavaScript, localStorage, and request-header injection, and HttpOnly
  keeps it invisible to page scripts. A same-engine (macOS WebKit) probe
  verified the `Set-Cookie` parse, store binding, automatic request
  attachment, and `document.cookie` invisibility before implementation.
- SwiftUI remounts the WKWebView by page identity
  (`.id(ObjectIdentifier(page))`), mirroring Android's `key(page)`; page
  events re-render through a `revision` state read in `body`, because
  SwiftUI invalidation only tracks state reads that happen during `body`
  evaluation.
- A revalidation 401 shares the native session revocation path with the
  web-page 401 path, so native and web sessions expire together.
- The external-link confirm alert is presented by the full-screen cover's own
  content, not by the presenting `RuntimeBrowserHost`. When an alert attached
  to the presenting view is dismissed while the cover is up, SwiftUI tears the
  cover down with it (verified by the UI-test screen recording: after Cancel
  the app returned to the conversation). Hosting the alert inside the cover
  scopes it to the browser session; the host-level alert still handles
  conversation-origin external links when no cover exists.

## Preserved boundary

- OIDC login keeps using `ASWebAuthenticationSession`; the runtime browser
  serves only already-authenticated runtime destinations.
- WKWebView has no `shouldInterceptRequest` equivalent for cross-origin
  subresources. iOS compensates with same-origin subframe policy, a host-only
  cookie, and `SameSite=Lax`; Holon App pages additionally run under the
  runtime app CSP (`default-src 'self'; connect-src 'self'`). Runtime-side CSP
  hardening for the web GUI stays a runtime topic (recorded per the Stage 2
  audit follow-up; no iOS-side change).
- The unit-test host cannot host WKWebView networking (the networking process
  crashes in the simulator test host), so pure policy and lease semantics are
  unit tested while the WKWebView path is covered by the simulator UI fixture.
