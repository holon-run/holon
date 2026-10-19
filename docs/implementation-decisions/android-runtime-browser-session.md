# Android hosted-App browser sessions

## Decision

Use the existing revocable opaque native session in a fresh AndroidX WebKit
Profile, bound through its CookieManager as a host-only, HttpOnly, SameSite=Lax
cookie. Add Secure on HTTPS. Do not pass bootstrap/control tokens, expose a
JavaScript bridge, invent a mobile authentication protocol or share the system
browser's cookie jar.

This follows the standard Cookie trust boundary approved for issue #3603:
cookies do not isolate ports. Services on the authentication hostname must be
trusted together. Exact-origin link routing and initial request interception are
navigation controls, not a guarantee about every redirect hop's credentials.
The regression suite explicitly demonstrates both same-host cross-port Cookie
delivery and cross-host exclusion, rather than claiming port isolation.

## Lifecycle and preserved boundaries

Each opening receives a unique Profile; connection/account/authentication
replacement retires its lease. Ordinary foreground metadata refresh is not an
authentication replacement and preserves the page. A rollback creates a fresh
lease so A→B→A cannot resurrect a retired page.

Destroy WebViews before asynchronous browsing-data cleanup, including workers.
Never reuse Profile names. On cold start, delete this feature's unloaded Profile
namespace before opening another page. Android providers must support both
MULTI_PROFILE and DELETE_BROWSING_DATA; no default-profile fallback is allowed.
This does not promise zero disk writes or immediate physical erasure.

Closing a page does not log out. Browser logout revokes the shared native
session; 401 returns to the existing native authentication flow. Hosted Apps
retain the runtime's existing same-origin authorization, CSP, CSRF and external
input provenance. Profile separation is not a new App-level permission sandbox.
