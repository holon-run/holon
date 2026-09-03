# OIDC Authentication Acceptance Case

This case is the repeatable black-box acceptance procedure for Issue #2735.
It is intentionally separate from release-level Docker and real-LLM cases.
Run it against a freshly built Holon image and isolated temporary state.

## Preconditions

- Build the candidate image from the exact commit under test:

  ```bash
  docker build -t holon:oidc-case .
  ```

- Use a unique Docker network, temporary `HOLON_HOME`, volume, and published
  port for every run. Do not reuse an existing database or session cookie.
- Start a standards-compatible local OIDC provider with a test client and test
  user. The provider must expose discovery, authorization, token, and JWKS
  endpoints. Keep provider secrets and tokens out of logs and evidence.
- If the provider cannot produce controlled negative responses, run a second
  local fixture that can return deliberately invalid issuer, audience, nonce,
  expiry, and signature values.

## Positive OIDC flow

Configure `auth.mode=oidc`, issuer, client ID, client-secret environment
variable, callback URI, and a short session TTL. Then verify:

1. `GET /api/auth/oidc/start` redirects to the provider and includes
   `state`, `nonce`, and an `S256` PKCE challenge. The response does not expose
   client credentials.
2. Complete authorization at the provider and call
   `/api/auth/oidc/callback?code=...&state=...`.
3. The callback exchanges the code with the PKCE verifier, fetches JWKS, and
   validates the RS256 ID Token (`iss`, `aud`, `exp`, `iat`, `nbf`, `nonce`,
   and signature).
4. The response sets the Holon session cookie with `HttpOnly` and the
   deployment-appropriate `Secure` and `SameSite` attributes.
5. Use the resulting session cookie for Web/API and the session bearer for API
   and SSE. All transports resolve the same local principal.
6. Log out through `POST /api/auth/session/logout`; verify cookie clearing,
   server-side revocation, and rejection of the old cookie and bearer.

## Local/static credential flow

Run this independently from OIDC login:

- valid bootstrap/recovery/static credentials work only at their documented
  exchange or control entry points;
- missing and incorrect credentials return the authentication failure contract;
- static credentials do not silently create an OIDC session;
- restart, expiry, revocation, and logout do not revive an invalid credential;
- remote TUI uses an explicit bearer token, while local TUI uses the local
  runtime/control-token path; neither is expected to consume a browser cookie.

## Negative matrix

For each row, record only status, error code, and sanitized headers:

| Scenario | Expected result |
| --- | --- |
| No cookie or bearer | `401`, `auth_required` |
| Random, expired, revoked, or disabled session | `401`, `auth_required` |
| Missing/wrong callback `code` or `state` | Login rejected; no session |
| Replayed or mismatched `state` | Login rejected; transaction consumed |
| Missing or mismatched `nonce` | Login rejected; no session |
| Wrong ID Token `iss` or `aud` | Login rejected; no session |
| Expired/not-yet-valid ID Token | Login rejected; no session |
| Invalid RS256 signature or unknown `kid` | Login rejected; no session |
| Authenticated but disallowed operation | `403` business-policy response |

## Evidence and cleanup

Save command lines, status/headers with secrets redacted, cookie attributes,
SSE connection results, provider/Holon image identifiers, and relevant bounded
logs. Never save ID tokens, access tokens, client secrets, session credentials,
or the callback capability URL. Stop and remove the Holon/provider containers,
network, volume, and temporary directory after every run, including failures.

The Rust regression that guards the RSA crypto-provider dependency is:

```bash
cargo test oidc::tests::validates_a_real_rs256_id_token_signature
```
