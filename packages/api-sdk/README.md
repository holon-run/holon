# `@holon/api-sdk`

The small, transport-oriented HTTP client shared by Holon SDK packages. It
handles base URL resolution, JSON request/response encoding, bearer
authentication, custom headers, cancellation, and structured HTTP errors.
`requestBinary()` uses that same transport and returns a `Blob`, response
headers (including content type and ETag), and status without text decoding.
Cookie behavior remains controlled by the supplied fetch implementation.

It intentionally does not define conversation or hosted-App semantics.
