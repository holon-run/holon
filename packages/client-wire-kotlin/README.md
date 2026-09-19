# Holon Kotlin client wire types

This directory contains generated Kotlin transport models for the Android
client. The source of truth is `docs/website/reference/openapi.json`.

The package is source-only: it is not a standalone SDK or published Maven
artifact. Android code should keep authentication, compatibility checks,
recovery, caching, and domain adapters outside these generated wire models.

Consumers using `kotlinx.serialization` must decode wire responses with unknown
keys enabled:

```kotlin
val wireJson = Json {
    ignoreUnknownKeys = true
}
```

Holon error envelopes can flatten endpoint-specific extension fields into the
top-level JSON object. Those fields are intentionally not all represented by
the generated `ErrorResponse` model, so the default strict `Json` configuration
can reject otherwise valid server responses.

Run `make transport-types` from the repository root to refresh the files.
`make transport-types-check` verifies that the OpenAPI snapshot and generated
TypeScript/Kotlin types are current. `make transport-types-kotlin-check`
compiles the generated Kotlin sources with the versions used by CI.
