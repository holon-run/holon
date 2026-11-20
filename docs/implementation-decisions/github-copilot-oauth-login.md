# GitHub Copilot OAuth login boundary

## Decision

Holon exposes GitHub Copilot as an experimental bring-your-own-credential
provider (`github-copilot@default` and `github-copilot@responses`) that
authenticates through the official GitHub device flow with a bundled public
third-party OAuth app (the opencode project's app) by default, and calls the
official `api.githubcopilot.com` surface. Holon does not embed the
Copilot SDK, does not reuse IDE credential stores, does not impersonate a
first-party client, and does not expose Copilot as a resold subscription proxy.

## Evidence

- Official GitHub and Microsoft implementations use `api.githubcopilot.com`
  for OpenAI-compatible model and chat requests. This is implementation
  evidence, not a complete public HTTP contract or an assurance that GitHub
  admits every user-owned app. Holon does not implement a code-completion API
  or guarantee the same models as first-party Copilot surfaces.
- The official device flow contract lives at fixed endpoints
  (`https://github.com/login/device/code` and
  `https://github.com/login/oauth/access_token`), requires no client secret,
  and reports pending, slow-down, denial, and expiry through OAuth-standard
  error codes. GitHub's scope list has no `copilot` scope, so Holon requests no
  scope and lets the user's OAuth app configuration decide.
- The Copilot chat and model endpoints live at the API root
  (`/chat/completions`, `/models`), not under a `/v1` prefix. Holon's generic
  OpenAI-compatible URL builder therefore does not apply to this provider.
- The official implementations send a GitHub user token directly as the
  Copilot bearer. Holon follows that request shape, but has not verified
  admission for a user-owned app or the `holon` integration identity.
  The internal `copilot_internal` token exchange has no public contract for
  third-party apps, so Holon does not use it.
- Copilot treats the OAuth app identity (client id) as an admission input.
  Probes on 2026-11-20 with one account showed Holon's earlier user-owned app
  reaching only a restricted legacy model set on the direct HTTP path, while
  the opencode project's public app reached the full current catalog
  (53 models) on the same path, and one minimal `/responses` inference with
  that app's device-flow token completed successfully. Header and endpoint
  variations did not change the restricted app's catalog. Holon still sends
  its own identity (`Copilot-Integration-Id: holon`) instead of a first-party
  editor identity,
  and users can override it through the normal provider header configuration.
  Which identities GitHub accepts for a given account remains server-side
  behaviour that Holon does not guarantee.

Sources: [GitHub device flow](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps),
[GitHub's authentication matrix](https://github.com/github/gh-aw-firewall/blob/main/docs/auth-matrix.md),
and [VS Code's Copilot request implementation](https://github.com/microsoft/vscode/blob/main/src/vs/platform/agentHost/node/shared/copilotApiService.ts).
The SDK's support for user-owned tokens and named partner integrations does
not establish a general direct-HTTP API admission contract for Holon.

## Preserved boundary

Holon bundles the public client id `Ov23li8tweQw6odWQebz` of the opencode
project's OAuth app, with the operator's explicit authorization, so users do
not need to register or enter an app id before login. The earlier bundled
user-owned app (`Ov23lioel2hHZITYyzwB`) is superseded: GitHub restricted it to
a legacy model set. Credential profiles record the client id used at login
and keep it across refreshes, so a login created with the old app must repeat
device login to move to the new default. Users can override the bundled app
with their own GitHub OAuth app:
`providers.github-copilot.auth.client_id` takes precedence over the
`GITHUB_COPILOT_CLIENT_ID` environment variable, then the bundled default.
Blank overrides are ignored. Holon records the selected client id in its
credential profile so refresh works without ambient configuration. No client
secret or user token is bundled, and no first-party client identity is used.

The model list is discovered from the account rather than declared in Holon's
catalog, so Holon does not claim which models a given subscription can reach.
Usage is metered and limited by GitHub's own subscription, quota, and policy
rules. A rejected credential, revoked authorization, or exhausted quota
surfaces as an explicit authentication or quota failure rather than a silent
retry loop: the Chat Completions transport forces one credential refresh and
replays the request once after an authentication failure, and a refresh that
the server rejects ends the turn with the credential error.

Holon verifies the request shape, the credential lifecycle, and the failure
handling with deterministic tests. After device login, the web client waits for
the credential reload generation before refreshing the discovered model list.
Reload errors and model-catalog refresh HTTP errors remain visible without
restarting the completed authorization poll. Individual provider discovery
failures still follow the model-catalog endpoint's existing error handling.

A limited live check with a user-owned OAuth app completed device authorization
and retrieved `/models` using Holon's integration identity. This verifies only
the tested app/account combination, not general Copilot entitlement, chat or
tool-call access, subscription quotas, or a guaranteed model set. Successful
device login alone confirms GitHub authorization, not Copilot model access.
A later live check with the bundled opencode app completed device login,
retrieved the full 53-model catalog on both the generic and account-derived
API bases, and completed one minimal `/responses` inference (`gpt-6.2-sol`).
