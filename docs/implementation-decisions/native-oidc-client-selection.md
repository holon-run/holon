# Native OIDC client selection

`/api/auth/oidc/native/start` accepts an optional `client` query parameter.
Omission and `android` select `run.holon.android://oidc/callback`; `ios`
selects `run.holon.ios://oidc/callback`. All other values, including empty
values and arbitrary callback URIs, are rejected with HTTP 400 before provider
discovery. Values are case-sensitive.

This is a fixed delivery-channel allowlist, not application authentication.
The existing state, independent S256 native proof, two-minute one-use ticket,
and atomic verifier-checked exchange remain unchanged. A wrong verifier must
not consume the ticket. The provider's configured redirect URI is separate
from these native callbacks and is not selected by this parameter.

The native start route is not currently defined in the OpenAPI source; this
change does not introduce a partial generated authentication contract.
