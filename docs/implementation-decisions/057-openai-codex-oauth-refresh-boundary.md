# OpenAI Codex OAuth Refresh Boundary

Holon-owned `openai-codex` OAuth credentials are refreshed only when they come
from the Holon credential profile store.

The provider may reuse the Codex CLI OAuth client id and token endpoint for
compatibility, but the persisted credential owner stays Holon. Refresh token
rotation is written back to `~/.holon/credentials.json` under the configured
OAuth profile, protected by a per-store lock, and never written back to
`~/.codex/auth.json` or the Codex CLI keychain.

Codex CLI credentials remain an external bootstrap/fallback source. If an
external CLI access token is expired, Holon reports an auth diagnostic instead
of silently refreshing or overwriting the Codex CLI store. Refresh failures such
as `refresh_token_expired`, `refresh_token_reused`, or revoked tokens are
surfaced as relogin-required Holon diagnostics.

If OpenAI/Codex later provides an official external-tool OAuth client or stable
refresh API, Holon should migrate this compatibility path without changing the
credential ownership boundary.
