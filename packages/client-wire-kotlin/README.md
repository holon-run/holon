# Holon Kotlin client wire types

This directory contains generated Kotlin transport models for the Android
client. The source of truth is `docs/website/reference/openapi.json`.

The package is source-only: it is not a standalone SDK or published Maven
artifact. Android code should keep authentication, compatibility checks,
recovery, caching, and domain adapters outside these generated wire models.

Run `make transport-types` from the repository root to refresh the files.
`make transport-types-check` verifies that the OpenAPI snapshot and generated
TypeScript/Kotlin types are current. `make transport-types-kotlin-check`
compiles the generated Kotlin sources with the versions used by CI.
