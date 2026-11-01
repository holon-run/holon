---
title: Connect to a remote Holon runtime
summary: Reach a runtime from another computer or mobile phone and retrieve task results.
order: 160
---

# Connect to a remote Holon runtime

Run Holon on a server or host machine and connect from your laptop, desktop, or
mobile device. Whether you want mobile access to your personal assistant or
are deploying a shared assistant for your team on a central server (see
[Share AI assistants with your team](./share-with-your-team.md)), Holon lets
the runtime stay active remotely while you interact from any client.

## Prerequisites

- Holon is installed on the runtime host and, for TUI access, on the client.
- You can administer the host's network and restart its daemon.
- Prepare a control token file on the host, readable only by the account
  running Holon. See [Providing a Token](#providing-a-token).
- Choose a network path the client can reach. For public access, use HTTPS.

Run server commands below on the runtime host. Choose either a managed daemon
or a foreground `holon serve` process; do not start both on the same port.

## Access Modes

Both `holon daemon start` and `holon serve` accept an `--access` flag:

| Mode | Description | Use Case |
|------|-------------|----------|
| `local` | Loopback only (127.0.0.1) | Default; single-machine use |
| `lan` | Local network | Same LAN, known IP |
| `tunnel` | Cloudflare Tunnel | Public access through a tunnel |
| `tailnet` | Tailscale network | Private mesh between your devices |

## Remote Server

### Tunnel Mode (Cloudflare)

Start a daemon accessible through a Cloudflare Tunnel:

```bash
holon daemon start --access tunnel --token-file ~/.holon/remote.token
```

Or use the standalone server:

```bash
holon serve --access tunnel --token-file ~/.holon/remote.token
```

The runtime manages the tunnel lifecycle. No Cloudflare configuration is
required on your side — Holon creates and manages ephemeral tunnels
automatically. Use the URL reported by the runtime rather than assuming that
its local listen address is the public endpoint. The tunnel exposes the
control plane through a third party; keep Holon authentication enabled.

### Tailnet Mode (Tailscale)

For private mesh access between your own devices:

```bash
holon daemon start --access tailnet --token-file ~/.holon/remote.token
# Or run in the foreground:
holon serve --access tailnet --token-file ~/.holon/remote.token
```

Requires Tailscale to be installed and authenticated on the host machine.
The client must also have tailnet access to the host. Tailnet connectivity and
Tailscale Serve HTTPS are separate choices; use the scheme and URL actually
configured for your endpoint.

### LAN Mode

For same-network access with a known IP:

```bash
holon serve --access lan --host 192.168.1.10 --port 8787 \
  --token-file ~/.holon/remote.token
```

### Custom Host and Port

Override the default listen address:

```bash
holon daemon start --access tunnel --port 9000 --token-file ~/.holon/remote.token
holon serve --access lan --host 0.0.0.0 --port 8787 \
  --token-file ~/.holon/remote.token
```

`0.0.0.0` listens on all IPv4 interfaces, potentially including public ones.
Binding an interface does not add TLS; restrict exposure with host networking
and use HTTPS when traffic leaves a trusted local path.

## Connecting Remotely

Use the host's advertised or configured base URL in place of the examples
below. A public HTTPS endpoint may use port 443 even when Holon listens locally
on another port. Open that same base URL in a browser for the Web GUI.

### TUI Connection

Connect from a remote terminal:

```bash
holon tui --connect https://your-server:8787 --token "your-token"
```

Read the token from a file:

```bash
holon tui --connect https://your-server:8787 --token-file ~/.holon/remote.token
```

Use a stored token profile:

```bash
holon tui --connect https://your-server:8787 --token-profile my-profile
```

### HTTP API

The same token authenticates HTTP control plane requests:

```bash
curl -H "Authorization: Bearer your-token" \
  https://your-server:8787/api/agents/list
```

See the [HTTP Control Plane reference](/reference/http-control-plane.md) for
the full API surface.

## Token Management

### Providing a Token

For direct CLI startup, provide your own control token. Pass it to the server
with `--token`, `--token-file`, or the
`HOLON_CONTROL_TOKEN` environment variable:

```bash
# Read the control token from a file
holon daemon start --access tunnel --token-file ~/.holon/remote.token
```

Prepare this file using a securely generated secret. Do not commit it, include
it in logs, or share it in plaintext.
Transfer the token securely to the client, or use
[device pairing](connect-android-client.md#option-a-qr-code-pairing-recommended)
for supported clients. Prefer a token file over an inline command-line secret.

### Token Profiles

Store multiple tokens as named credential profiles, then select one by name:

```bash
holon config credentials set office --kind bearer_token --stdin
holon config credentials set home --kind bearer_token --stdin
```

Then connect by profile name:

```bash
holon tui --connect https://office:8787 --token-profile office
holon tui --connect https://home:8787 --token-profile home
```

### Mobile Access (Android & iOS)

You can also connect to remote runtimes from a mobile phone or tablet using
native Holon mobile clients. Provide your remote HTTPS base URL and token or QR
code pairing to start an encrypted session:
- [Connect with the Android client](/guides/connect-android-client.md) — install the APK or build from source
- [Connect with the iOS client](/guides/connect-ios-client.md) — connect from iPhone or iPad


## Daemon Management

Run these commands on the runtime host, for example through an authorized SSH
session. They manage that host's daemon; they do not target the server selected
by a previous `holon tui --connect` command.

```bash
holon daemon status
holon daemon logs
holon daemon restart
holon daemon stop
```

## Verify the connection

1. Open the remote GUI or connect with the TUI and confirm the expected agent
   roster. Check the base URL to avoid testing a different runtime.
2. Send a harmless request to a known agent and wait for its new response.
   A page loading or a request being accepted is not proof that the agent
   completed the work.
3. **Retrieve an existing task:** From the remote client, navigate to an active
   or recent WorkItem by ID. Verify that you can read the latest brief and
   output artifacts generated on the server host.
4. Request `/api/agents/list` with the configured credential and check for a
   successful JSON response.
5. Repeat the request without a credential:

   ```bash
   curl -i https://your-server:8787/api/agents/list
   ```

   Expect an authentication error, not the agent list. If protected data is
   returned, stop exposing the endpoint until authentication is corrected.

## Troubleshooting

- **Connection refused or timed out:** Check the host's daemon status, listen
  address, port, firewall, and the client's network path.
- **Tailnet host unreachable:** Check that both devices are connected and that
  tailnet access rules allow the connection.
- **Authentication error:** Check the client credential and server auth mode.
  For OIDC deployments, follow organization login rather than assuming a shared
  control token is a user session.
- **GUI loads but API fails:** Serve the GUI and API at the same origin's root
  or check the reverse proxy and CORS configuration.
- **Tunnel URL stopped working:** Check the current advertised URL and tunnel
  status on the host; do not assume an ephemeral URL survives a restart.

## Security Considerations

- **Always use a token**. Holon refuses to start with a non-loopback listen
  address, or with `--access lan`/`--access tailnet`, unless a token is set.
  Set a token for `--access tunnel` too: the tunnel is publicly reachable.
- **Prefer tunnel or tailnet** over exposing a raw LAN listener to the internet.
  Keep Holon authentication enabled; network access does not replace it.
- **Rotate tokens** by restarting the daemon with a new `--token-file`.
- **Use `--access local`** when only local connections are needed. This is
  the default and the most secure option.
- The HTTP control plane applies trust-boundary rules: read-only routes
  (agent state, events, tasks) still require a valid token for remote access.
- **Team deployments**: For shared multi-user deployments, switch to OIDC
  authentication (`auth.mode = "oidc"`). This replaces the shared control token
  with individual SSO logins. See [Configure OIDC authentication](/guides/configure-oidc-authentication.md).

Remote authentication controls entry to the runtime. It does not sandbox agent
commands or create filesystem isolation between users. Read
[Security and execution boundaries](../concepts/security-and-execution-boundaries.md)
before exposing a host with sensitive files.

## See Also

- [TUI reference](/reference/tui.md) — navigation, slash commands, and remote connect
- [HTTP control plane](/reference/http-control-plane.md) — API reference for programmatic access
- [Troubleshoot a Holon task](/guides/troubleshooting.md) — connection problems
- [Automate Holon over HTTP](/guides/automate-over-http.md) — drive Holon from code
- [Connect with the Android client](/guides/connect-android-client.md) — manage agents from mobile devices
- [Connect with the iOS client](/guides/connect-ios-client.md) — inspect tasks from iOS devices
- [Configure OIDC authentication](/guides/configure-oidc-authentication.md) — single sign-on and audit trails
