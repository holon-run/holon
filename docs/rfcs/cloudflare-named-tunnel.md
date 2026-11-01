# Managed Cloudflare Named Tunnel

## Status and scope

Accepted for #3865 on 2026-11-01: **Named Tunnel only**, replacing the issue's
Quick Tunnel proposal. Quick Tunnels do not support SSE; existing conversation
SSE and clients remain unchanged.

## Configuration and authority

`serve` and `daemon start` / `restart` accept `--access cloudflare`,
`--cloudflare-token-file PATH` and `--advertise https://HOSTNAME`.
Install `cloudflared` and preconfigure a dedicated remotely-managed Named
Tunnel, public hostname and ingress targeting Holon's fixed loopback port.
Holon does not create Cloudflare resources, change DNS, override remote
ingress with `--url`, or consume an account API token. The credential file
contains a **tunnel connector token**, separate from the Holon control token.

Require a valid Holon control token and force `Required` authentication even
on loopback or with inherited disabled policy. Reject non-loopback origins,
ephemeral ports, non-HTTPS URLs, URL credentials, paths, queries, fragments
and Quick Tunnel hostnames. Never print either credential value.
The connector grants access to its tunnel's remote configuration: use only
a dedicated tunnel with trusted ingress rules. Validating Holon's listener
does not validate the account's complete remote ingress policy.

## Durable intent

Add the optional absolute `cloudflare_token_file` path and `cloudflare`
access value to the v1 serve profile. Old profiles retain their meaning.
Persist and forward only the file path, never connector token contents.
Revalidate credentials at startup. Unqualified restart restores the accepted
public exposure. Explicitly changing access removes inherited Cloudflare
intent but does not erase the Holon authentication requirement.

## Lifecycle and observation

Check dependencies and credentials before runtime recovery. Launch after the
fixed loopback listener is bound, with cloudflared self-update disabled.
The child guard terminates and reaps on normal shutdown, SIGINT and SIGTERM.
Linux additionally protects against parent death; SIGKILL cleanup is not
guaranteed on every platform.

Runtime readiness/status and CLI daemon status expose a separate optional
`tunnel` observation: phase, configured public URL and fixed non-secret
message. Local healthy does not mean public healthy. `connected` means an
edge connection is registered, not that DNS, ingress, authentication or SSE
have passed end-to-end checks. Timeout, disconnect and exit remain observable
without crashing the local runtime. No hidden process restart or fallback to
Quick Tunnel. Bound startup waits and log reads; discard raw connector logs.

## Verification

Test CLI/auth/listener/URL validation, v1 compatibility, profile forwarding,
restart and mode replacement. Fake subprocesses cover connection, failure,
timeout and cleanup. Separately validate a real isolated Named Tunnel:
unauthenticated control rejection, authenticated Web/handshake, incremental
SSE before stream closure and connector cleanup. Remove only test-created
resources; never attach a test connector to a production tunnel.
