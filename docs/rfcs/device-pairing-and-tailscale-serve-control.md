# Device Pairing and Tailscale Serve Control

## Status

Accepted for the Web pairing and shared Serve control slice, September 28, 2026.

## Pairing entry points

The local-mode daemon owns short-lived, single-use pairing tickets. The macOS
menu and an authenticated Web Settings page are both clients of
`POST /api/auth/pairing/issue`; neither stores or embeds the long-lived control
token in a QR code. Issuance happens only after a user gesture. Both clients
show the destination address, prefer tailnet HTTPS when available, warn about
plaintext LAN traffic, and remove an expired code. Linux operators can use the
Web entry point without installing the macOS menu.

The Web client derives the destination from its current authenticated origin,
not a server-provided LAN address or an untrusted URL parameter. A pairing link
uses the existing `/login#pair=<ticket>` redemption path. Hiding a code does not
invalidate the ticket immediately; its two-minute expiry and single-use
redemption remain the daemon's authority.

## Shared Serve state

The daemon exposes control-authenticated Serve status and explicit enable and
disable operations. The operator's desired `enabled` value is runtime-wide
configuration in the Holon home `config.json`, distinct from the current
Tailscale rule. A status request inspects Tailscale's actual rule and reports
whether it serves Holon, conflicts with another root handler, or is absent or
unavailable. An externally removed rule leaves the desired value untouched;
clients show the drift and offer **manual** recovery, not a background retry.

`GET /api/control/network/tailscale/serve` returns `desired_enabled`,
`available`, `connected`, `status_known`, `serving`, `conflict`, `hostname`,
`serve_url`, and `message`. `POST` to its `/enable` or `/disable` suffix
explicitly changes the rule and, on success, saves the desired value. Status
inspection never changes Serve. When the current root rule cannot be inspected,
`status_known` is false and both mutations are refused.

Only an explicit enable may install the root Serve rule. It must reject an
existing root handler for another service without replacing it. Disable may
remove only a root rule known to point to this daemon; it must not reset other
Tailscale configuration. Mutations target the current tailnet host's HTTPS 443
`/` handler; other paths and hosts are left untouched. Failed operations must
not change the desired value.
The Serve backend uses the daemon's local loopback listener on the HTTP port,
not its advertised LAN address. Enabling Serve requires effective Holon TCP
control authentication: a configured nonempty control token in local mode
(with control authentication enabled), or OIDC session authentication. Existing
valid sessions remain usable; no per-request token prompt is required. A root
rule pointing to the current LAN listener is recognized as Holon's legacy rule,
reported as needing migration, and replaced by the loopback target only on
explicit enable; unrelated root rules remain conflicts.
The menu and Web Settings are clients of the same daemon operations, not owners
of independent saved preferences or competing Serve reconciliation loops.
Tailscale remains optional; Holon does not manage Tailscale installation, login,
ACLs, certificates, or Funnel.

This shared-control slice extends the manual deployment guidance in issue
#3216; it does not make Tailscale a core runtime dependency or authorize
automatic repair of externally modified network configuration.
