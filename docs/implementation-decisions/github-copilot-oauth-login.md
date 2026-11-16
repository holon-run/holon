# GitHub Copilot OAuth login boundary

## Decision

Holon supports GitHub Copilot as an optional bring-your-own-credential
provider (`github-copilot@default`) that authenticates with the user's own
GitHub OAuth app through the official GitHub device flow, and calls the
official `api.githubcopilot.com` chat surface. Holon does not embed the
Copilot SDK, does not reuse IDE credential stores, does not impersonate a
first-party client, and does not expose Copilot as a resold subscription proxy.

## Evidence

- GitHub documents `api.githubcopilot.com` as the OpenAI-compatible model and
  chat surface for third-party clients, metered against the user's own Copilot
  subscription and AI credits. There is no completion-level API, and model
  availability for third-party OAuth apps is governed separately from the
  first-party Copilot surfaces.
- The official device flow contract lives at fixed endpoints
  (`https://github.com/login/device/code` and
  `https://github.com/login/oauth/access_token`), requires no client secret,
  and reports pending, slow-down, denial, and expiry through OAuth-standard
  error codes. GitHub's scope list has no `copilot` scope, so Holon requests no
  scope and lets the user's OAuth app configuration decide.
- The Copilot chat and model endpoints live at the API root
  (`/chat/completions`, `/models`), not under a `/v1` prefix. Holon's generic
  OpenAI-compatible URL builder therefore does not apply to this provider.
- A user-owned OAuth app token is accepted directly as the Copilot bearer.
  The internal `copilot_internal` token exchange is a first-party IDE path
  with no public contract for third-party apps, so Holon does not use it.
- Copilot requires an integration identity header and treats it as a routing
  input, not decoration: different identities expose different model sets.
  Holon sends its own identity (`Copilot-Integration-Id: holon`) instead of a
  first-party editor identity, and users can override it through the normal
  provider header configuration when their plan requires another identity.

## Preserved boundary

The OAuth client id belongs to the user's own GitHub OAuth app. Holon reads it
from `providers.github-copilot.auth.client_id` (or the
`GITHUB_COPILOT_CLIENT_ID` environment variable), records it in the Holon
credential profile so refresh works without ambient configuration, and never
ships a bundled client id. Without a client id, login fails with an explicit
message instead of silently falling back to a first-party identity.

The model list is discovered from the account rather than declared in Holon's
catalog, so Holon does not claim which models a given subscription can reach.
Usage is metered and limited by GitHub's own subscription, quota, and policy
rules, and a rejected credential, revoked authorization, or exhausted quota
surfaces as an explicit authentication or quota failure rather than a silent
retry loop.
