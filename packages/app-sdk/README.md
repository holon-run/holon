# `@holon/app-sdk`

The browser-facing facade for a hosted Holon App. It depends on
`@holon/api-sdk` for HTTP transport and is designed to compose with
`@holon/conversation-sdk` without exposing the latter's protocol surface as the
App global.

The hosted artifact is `dist/holon.js`; Holon serves it from the logical
`/apps/{agent_id}/{app_id}/holon.js` route. The artifact discovers its own
route-relative base URL and installs `window.Holon`.
