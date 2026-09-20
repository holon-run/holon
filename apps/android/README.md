# Holon Android

This directory contains the native Android client implementation.

The first checked-in module is `:sdk`, a Kotlin/JVM library that keeps generated
wire models separate from stable client-facing domain models. It deliberately
does not include a Compose application, Android credentials, signing, or
distribution configuration.

The generated transport sources remain owned by
`packages/client-wire-kotlin`. Do not copy or edit those models in this
directory.

Run the SDK tests from the repository root:

```sh
make android-sdk-test
make android-sdk-integration-test
```

The checked-in Gradle Wrapper pins Gradle 8.14.3. A JDK 21 installation is the
only required local build prerequisite; no system Gradle installation is
needed. The integration target also builds and starts the repository's real
`holon` daemon for the read-only handshake and Agent roster contract.

`HolonHttpClient` accepts the API base URL. For a directly connected daemon,
use an address ending in `/api`; reverse proxies may supply another API prefix.
