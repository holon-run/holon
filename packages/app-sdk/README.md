# `@holon/app-sdk`

The browser-facing facade for a hosted Holon App. It depends on
`@holon/api-sdk` for HTTP transport and is designed to compose with
`@holon/conversation-sdk` without exposing the latter's protocol surface as the
App global.

The hosted artifact is `dist/holon.js`; Holon serves it from the logical
`/apps/{agent_id}/{app_id}/holon.js` route. The artifact discovers its own
route-relative base URL and installs `window.Holon`.

The `/context` response reports whether the existing control-plane session is
authenticated. It does not expose App-specific permissions or capability
grants; the current Local App routes use the same broader session authorization
for context, requests, and events.

`events()` returns an async iterator backed by `EventSource`. A source error,
`AbortSignal`, or iterator `return()` ends that iterator and closes its source;
callers that want to retry must create a new iterator. The browser `Holon.events`
callback facade forwards source errors to its optional `onError` callback.

The SDK does not promise explicit initial replay from `lastEventId`; native
`EventSource` reconnection behavior remains separate from the async iterator's
fail-fast error contract.
