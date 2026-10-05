# Android client boundaries before the iOS client

Keep the Android transport as a facade, but put conversation semantics in the
pure JVM SDK: revision-aware canonical ordering, active turns outside the page
window, independent history/stream cursors, and publication only after a complete
batch checkpoint. Unknown control messages require a new authoritative snapshot;
they are never silently applied. The mobile v2 fixture under
`tests/fixtures/client-wire` is a cross-client conformance input.
When a live window would overflow, retain the published window until a new
bootstrap provides the authoritative history boundary. Never fabricate a history
cursor from the independent stream checkpoint.

The app owns credentials and lifecycle. SessionCoordinator binds a client
generation to a network/base URL/runtime/user/visibility scope. Transport errors
do not delete credentials. A late response from a replaced client cannot
invalidate the replacement. Native browser login binds the callback ticket to
the app's S256 proof; manual local-token login and explicitly confirmed HTTP
remain supported.

ForegroundSyncCoordinator owns one closable roster-hint connection; the selected
conversation owns one additional stream. Operator previews use bounded
concurrency, not one permanent stream per Agent. No background service, periodic
sync, or notification promise is introduced.

DurableOutbox separates send-state persistence from navigation and attachment
staging. It has injectable storage/transport seams and never regenerates the
request ID or content on an unknown-result retry. FilesRepository owns platform
file I/O; WorkRepository reads work details without projecting conversation
state. Room v4 removes cross-network uniqueness and preserves old data; saved
scope keys are moved transactionally without changing credential aliases.

Rendering and actions are split by feature. HolonUiState groups immutable feature
states, with a flat compatibility facade while the existing HolonViewModel
remains the lifecycle orchestrator. This is an incremental boundary change, not
a claim that every action has a separate ViewModel. It avoids a second mutable
state source while making future feature extraction independently reviewable.
SavedStateHandle keeps route identity/section only; drafts/outbox stay in Room
and Compose retains reading anchors. Inline turn/tool expansion is not a route.

Core connection/session/outbox copy uses Android resources; the existing
source-keyed translation adapter remains for untouched UI. Authored Agent
content is never translated. Swift should implement these contracts against the
shared fixtures, not translate the Android global ViewModel.

Verification is deliberately separate: ordinary JVM tests, explicit Paparazzi
verification, lint/build, real daemon SDK/auth checks, then emulator platform
tests. A screenshot baseline changes only after visual inspection; the current
tool result is rendered once rather than duplicated as summary plus result.
Store-readiness and iOS platform probes remain outside this refactor.
