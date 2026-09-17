# Model settings and selection — phase 1

## Scope and intent

Replace the long provider form directory with searchable connection management,
and replace the provider-first conversation picker with model-first search.
Reuse the existing runtime API and preserve canonical `provider@endpoint/model`
identity, credentials, fallback order, and agent override semantics.

## Design

Use the existing Holon typography, neutral surfaces, spacing and focus styles.
The identifying detail is the secondary source label beside every model: users
can distinguish API, subscription and custom endpoint routes without reading a
full canonical ID. Settings uses compact connection rows and one inline editor;
the conversation Context panel is independent.

- Settings shows connected/configured services by default. Credentials from env
  or external login count; saved configurations remain visible when credentials
  fail. Unconfigured credential-free builtins belong in All unless in use.
- Search matches friendly brand names, Chinese/English aliases, internal IDs,
  and route/plan labels. Explicit brand metadata is presentation-only; custom
  and unknown provider IDs have a safe fallback.
- All services is the discovery entry point. Selecting a row opens exactly one
  editor with Back navigation, existing authentication, save and remove actions.
  Keep draft edits when searching/navigating within the page.
- Distinguish credentials present, credentials missing and no-auth configuration;
  never imply a successful network probe from these facts.
- Reuse one searchable model list in the conversation, global default, fallbacks,
  image observation and generation settings. Settings retains advanced explicit
  route input for custom models. Search spans all eligible routes with an optional
  provider filter; default view prioritizes favorites and recent successful picks.
- Favorites and recents are browser preferences scoped by current API origin,
  capped and validated on read, synchronized across mounted pickers/tabs, and
  robust to malformed or unavailable localStorage. They do not disable models or
  change runtime routing. Unavailable selected routes stay visible with a reason.
- Model rows show display name, provider/plan/endpoint, capabilities, and a
  copyable canonical reference in details. Same-name routes never merge.
- Conversation selection persists an agent override, not a one-turn preference.
  Retain runtime-default reset and independent supported reasoning levels. Render
  active vs requested next-run selection from server facts, including after reload.

## Non-goals

No connection probes, new providers, account creation, model policy migration,
server-side favorite storage, or automatic model/fallback selection. No runtime
restart or live agent prompts are needed for verification.

## Acceptance

1. Dozens of providers require neither dozens of forms nor scrolling to search.
2. Chinese brand search finds each plan without changing provider IDs.
3. Search finds models across providers; favorite/recent ordering survives refresh.
4. Same model name on separate routes remains distinguishable and selects exactly
   the intended route. Unavailable routes cannot be submitted.
5. Global defaults, image capability filtering, ordered fallbacks, OAuth/API-key
   editing and agent override reset still work.
6. Active vs requested models display accurately for deferred switches.
7. Keyboard navigation, focus, narrow layouts, empty/error/stale states work.
8. Unit tests, frontend typecheck/build and fixture browser regressions pass;
   real credentials and the running daemon remain untouched.

## Delivery

Implement on `codex/model-settings-picker`, update this record with validation,
and open a PR against main. Phase 2 may add backend-supported connection tests
and richer diagnostics after separate discussion.


## Implementation and validation

Implemented with existing APIs only. The frontend now retains `effective_model`
and `runtime_default_model` beside the previously displayed `active_model`.
Next-run selection labels use the effective configuration; runtime fallback or
missing metadata does not silently rewrite the selection. Explicit manual route
entry remains available in settings for models outside the discovered catalog.
Large model groups render in batches of 60 with an explicit Show more action.

Validation:
- 607 frontend unit tests across 57 files passed, including alias search,
  route preservation, storage corruption/origin isolation and model projection.
- Five new Chromium scenarios passed: 43-provider directory/single editor/drafts,
  route-specific selection and refresh, image capability filtering/narrow layout,
  cross-tab favorites, and failed-switch selection preservation.
- Full Chromium regression first run: 42 passed, two existing timing-sensitive
  cases failed (conversation scroll anchoring and hydration settling). Their
  complete two files then passed all 14 cases with one worker, without changes
  to those tests or the relevant scrolling/sync implementation.
- Typecheck and E2E build passed; desktop and 390px screenshots inspected.
- Production build and production-only diagnostics exclusion checked before PR.

No Rust changes, schema changes, daemon restart, real credential mutations, or
live agent prompts. Browser verification used the isolated fixture server.
