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

The explicit custom destination takes precedence. Otherwise both clients prefer
a valid, authenticated Serve HTTPS origin, then the Web client's current
non-loopback origin or the menu's known LAN entry point. Serve must be available,
connected, known, actually serving Holon, and conflict-free. Its URL must be a
bare HTTPS origin matching its reported hostname; a desired enabled value or a
URL field alone is not proof of availability. The Web client does not select a
server-provided LAN address or an untrusted URL parameter.

The destination row explains that Serve requires the phone to join the tailnet
and have ACL access, and keeps LAN/custom selection available. A loopback-only
installation without Serve has no default phone destination. An HTTPS reverse
proxy can use the current browser origin or an explicit custom HTTPS origin;
`--advertise` is not required for Web pairing. The proxy must serve Web and API
at the same origin's root; subpath deployments are not supported by this flow.
OIDC users use normal login rather than local-mode pairing tickets. A pairing link
uses the existing `/login#pair=<ticket>` redemption path. Hiding a code does not
invalidate the ticket immediately; its two-minute expiry and single-use
redemption remain the daemon's authority.

Clients refresh network state before issuance and clear the displayed ticket on
destination changes, network mutations, daemon identity/address changes, unknown
or invalid status, and expiry. Clearing never issues a replacement automatically
or claims to revoke a server-side ticket.

## Shared Serve state

The daemon exposes control-authenticated Serve status and explicit enable and
disable operations. The operator's desired `enabled` value is runtime-wide
configuration in the Holon home `config.json`, distinct from the current
Tailscale rule. A status request inspects Tailscale's actual rule and reports
whether it serves Holon, conflicts with another root handler, or is absent or
unavailable. An externally removed rule leaves the desired value untouched;
clients show the drift and offer **manual** recovery, not a background retry.
Tailscale's HTTPS listener marker on TCP 443 accompanies a normal HTTPS Serve
rule and is not itself a conflict; a separate TCP forwarding listener is.

`GET /api/control/network/tailscale/serve` returns `desired_enabled`,
`available`, `connected`, `status_known`, `serving`, `conflict`, `hostname`,
`serve_url`, `control_authentication_available`, and `message`.
`control_authentication_available` reports effective TCP authentication without
exposing a credential; disabled control authentication is not available.
`POST` to its `/enable` or `/disable` suffix
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
not its advertised LAN address. A `localhost:port` listener retains its hostname
target so the proxy resolves the same loopback address family as the listener.
Enable accepts only a configured primary listener that is loopback, `localhost`,
or wildcard, and rejects numeric LAN/tailnet primary addresses before changing a
rule. This is a configuration policy, not a claim that CLI-started numeric
listeners lack loopback: the CLI already adds a separate IPv4 loopback listener
for these addresses. That existing dual-listener behavior remains unchanged;
Serve enable does not infer eligibility from auxiliary sockets.
Enabling Serve requires effective Holon TCP control authentication, regardless
of whether the request arrives over TCP or the Unix socket: a configured
nonempty control token in local mode
(with control authentication enabled), or OIDC session authentication. Existing
valid sessions remain usable; no per-request token prompt is required. A root
rule pointing to the current LAN listener is recognized as Holon's legacy rule,
reported as serving Holon and needing migration, and replaced by the loopback
target only on explicit enable; unrelated root rules remain conflicts.
The menu and Web Settings are clients of the same daemon operations, not owners
of independent saved preferences or competing Serve reconciliation loops.
Tailscale remains optional; Holon does not manage Tailscale installation, login,
ACLs, certificates, or Funnel.

This shared-control slice extends the manual deployment guidance in issue
#3216; it does not make Tailscale a core runtime dependency or authorize
automatic repair of externally modified network configuration.

## Independent controls and remote authentication preparation

LAN exposure, Serve exposure, desktop capability, and pairing destination are
separate decisions. The menu's LAN preset explicitly binds `0.0.0.0:<port>` and
advertises the known LAN IPv4 address. This includes loopback for menu API and
Serve, but also exposes all IPv4 interfaces, potentially including public ones;
the confirmation must say so and effective control authentication is mandatory.
Disabling LAN binds loopback without disabling Serve or discarding credentials.
CLI numeric `--host` binding semantics remain unchanged, and `--access tailnet`
is a direct HTTP access preset, not an instruction to enable Serve HTTPS.

An explicit menu enable-Serve gesture may prepare the private local-mode control
token and restart a menu-managed daemon to load it, after the user-facing
confirmation. Preparation preserves access, listener, port, and Finder opt-in;
it must not enable LAN as a side effect. The existing owner, regular-file,
no-follow, and private-permissions checks remain required. Subsequent managed
start/restart retains the approved credential. Externally managed or unknown
credentials are not overwritten or rotated. The Web client only explains
operator configuration; it never creates a long-lived token or restarts Holon.
If preparation succeeds but sharing fails, report that partial result; failed
Serve mutation must not change the desired enabled value.

Finder opt-in enables the daemon host's macOS capability, not a same-device API
restriction. Authorized remote API callers may reveal a validated path; the Web
UI hides the entry point outside localhost/loopback as a UX policy. Path,
authentication, opt-in, and desktop cross-site protections remain enforced.
Local mode without required credentials additionally accepts only a loopback
Host for reveal requests, preventing DNS rebinding from turning a same-origin
check into unauthenticated remote desktop access. Required local credentials
and OIDC permit remote Hosts, while still requiring a matching Origin.
This does not broaden the general Cookie/Origin/CSRF work tracked in #3315.
