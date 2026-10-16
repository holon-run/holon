# Daemon Serve Profile Persistence

## Status

Accepted for implementation for issue #3550 (2026-10-16). Linux verification
and macOS build / real-device verification are separate acceptance gates.

## Problem and ownership

`run/daemon.json` describes a process, not the next startup. Removing runtime
metadata after shutdown must not discard listener or authentication intent.
The lifecycle's `desired_running` boolean answers a different question: whether
the operator wants a daemon running.

The daemon owns a versioned `HOLON_HOME/daemon/serve_profile.json`. It contains
the accepted, non-secret serve configuration. `config.json` remains the
runtime-mutable configuration surface. Swift and service managers are clients,
not additional profile writers. This is not live listener reconfiguration.

## Resolution

All foreground `serve` and managed `daemon start` / `restart` paths share:

1. Explicit CLI options.
2. Explicit supported environment inputs (`HOLON_HTTP_ADDR`,
   `HOLON_CONTROL_TOKEN`, and the existing control-auth policy).
3. The persisted profile.
4. Validated legacy metadata, only when no profile exists.
5. Platform defaults.

An absent environment variable is not an override merely because `AppConfig`
has filled a default. `listen` / `port` and token / token-file are field groups;
a higher-priority member replaces the inherited alternative. Explicit binding
or access changes invalidate inherited advertised addresses. Authentication-only
changes preserve networking, web assets and desktop settings.

Save the complete resolved intent, not only this invocation's patch. Preserve
binding semantics rather than flattening every request into a `listen` string.
Resolve relative token-file and web-dist paths against the accepting process's
working directory. Home, socket, executable identity and credentials are not
inherited from the profile.

`--reset-serve-options` explicitly discards inherited serve options before
resolving CLI, environment and defaults. Corrupt or unsupported profiles fail
closed unless explicitly reset. Missing metadata is never an implicit reset.
Without a profile or valid migration source, report defaults and the absence
of recoverable history; previously deleted options cannot be reconstructed.

## Credentials and durable writes

Authentication intent records `none`, a token-file path, or `external_required`.
The profile also persists `control_auth_mode` as `auto`, `required` or
`disabled`, independently of the credential source. Removing the original
environment must not erase a saved local `required` policy.
Never persist token contents, inline tokens, environment tokens or raw argv.
A token file must be read and validated again on every startup. Missing or empty
credentials reject startup, including loopback startup when the saved intent
requires a token. Switching access to local does not erase that dependency.
An explicit credential replacement or reset is required.

Use owner-only Unix directory / file permissions (0700 / 0600), a private
same-directory temporary file, atomic replacement and supported durability
flushes. Reject unsafe symlink targets. Invalid schema or fields must not
silently become defaults.

The lifecycle lock serializes resolve, validate and accept:

1. Check arguments, paths, authentication and existing runtime identity.
2. Reject foreign ownership and incompatible idempotent Start without changing
   the saved profile or stopping a process.
3. Persist the profile, then `desired_running`, before stopping or spawning.
4. Keep accepted intent if binding or readiness subsequently fails. Report
   failure / non-convergence rather than claiming that the daemon is running.

The two files are not a cross-file transaction. This ordering can leave new
serve intent with old running intent after a crash, but cannot introduce new
auto-start intent without its corresponding serve profile.

A managed child consumes the parent's resolved launch plan without remerging
or reentering the parent's lifecycle lock. Foreground registration uses the same
serialization boundary, releasing it after publishing runtime metadata rather
than holding it through the server loop. Stop, shutdown cleanup and
prepare-update do not remove serve intent.
Stop and prepare-update validate running process identity independently of the
desired profile, so a malformed profile cannot prevent safe shutdown. Status
and new launches still report the malformed profile instead of using defaults.

## Observation and menu behavior

Status exposes an optional, secret-safe desired serve projection even when
stopped, and distinguishes its source from actual runtime observations.
Status reads do not migrate or write the profile.
The optional `desired_serve` object contains `source`, with values `profile`,
`legacy` or `defaults`. `defaults` confirms that neither a saved profile nor
valid recoverable legacy intent exists; malformed profiles fail instead of
reporting defaults. An absent object means an older binary or unknown intent,
not confirmed defaults. For `source: "defaults"`, menu clients may send the
request-only `--desktop-integration-if-unconfigured` hint, not an explicit
desktop override; unknown sources preserve ordinary inheritance.
The daemon applies this hint only when it reads neither a profile nor valid
legacy intent under the lifecycle lock. A status snapshot cannot establish that
the home is still unconfigured when a later launch is accepted. If another
launch saves intent in between, Start and Restart inherit that intent instead.
Explicit `--desktop-integration` wins over the hint; combining the hint with
`--reset-serve-options` is rejected. The hint is not saved or replayed in runtime
metadata or child arguments.

Startup diagnostics identify the base source and explicit CLI / environment
overrides by field and variable name, even when their values agree. Credential
diagnostics identify only the source, never the value. Do not log raw argv.
Service-manager overrides therefore remain visible instead of silently winning.

Ordinary menu Start, Restart, login recovery and update recovery inherit.
They do not reconstruct LAN settings from observed HTTP addresses or inject
desktop / authentication defaults. Only a confirmed first startup without
profile or legacy intent opts into desktop integration once. Explicit network
and authentication actions send patches through Rust. Existing credential
approval, session provenance and foreign-process ownership checks remain intact.

Regression coverage must interleave a defaults status observation, another
accepted launch with desktop integration disabled, and the original hinted
Start / Restart. The hint must not override the newly saved setting or cause an
otherwise identical Start to reject adoption.

## Compatibility and non-goals

Missing profile means pre-migration; valid legacy metadata can establish one at
an explicit accepted launch. Legacy external-token use becomes
`external_required`, not recovered credentials. Old binaries cannot inherit this
new file; downgrades require explicit startup parameters and credentials.

No launchd / systemd ownership changes, general configuration framework,
credential rotation, shell-history recovery, hot reload or production deployment.
Linux isolated-process tests do not establish macOS or actual machine reboot
acceptance.
