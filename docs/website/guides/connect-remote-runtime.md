---
title: Connect to a remote Holon runtime
summary: Reach a runtime running on another machine and verify the connection.
order: 15
---

# Connect to a remote Holon runtime

Run Holon on one machine and use it from another — a shared team server, a
headless box, or a remote dev host. This guide picks an access mode, connects,
and confirms the connection works.

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
holon daemon start --access tunnel
```

Or use the standalone server:

```bash
holon serve --access tunnel
```

The runtime manages the tunnel lifecycle. No Cloudflare configuration is
required on your side — Holon creates and manages ephemeral tunnels
automatically.

### Tailnet Mode (Tailscale)

For private mesh access between your own devices:

```bash
holon daemon start --access tailnet
holon serve --access tailnet
```

Requires Tailscale to be installed and authenticated on the host machine.

### LAN Mode

For same-network access with a known IP:

```bash
holon serve --access lan --host 192.168.1.10 --port 8787
```

### Custom Host and Port

Override the default listen address:

```bash
holon daemon start --access tunnel --port 9000
holon serve --access lan --host 0.0.0.0 --port 8787
```

## Connecting Remotely

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

Holon does not generate control tokens for you. Pick a secret yourself, then
hand it to the server with `--token`, `--token-file`, or the
`HOLON_CONTROL_TOKEN` environment variable:

```bash
# Read the control token from a file
holon daemon start --access tunnel --token-file ~/.holon/remote.token
```

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

## Daemon Management

Once the daemon is running, standard management commands work remotely:

```bash
holon daemon status
holon daemon logs
holon daemon restart
holon daemon stop
```

## Security Considerations

- **Always use a token**. Holon refuses to start with a non-loopback listen
  address, or with `--access lan`/`--access tailnet`, unless a token is set.
  Set a token for `--access tunnel` too: the tunnel is publicly reachable.
- **Prefer tunnel or tailnet** over LAN mode when connecting across the
  internet. These provide encryption and authentication without exposing raw
  ports.
- **Rotate tokens** by restarting the daemon with a new `--token-file`.
- **Use `--access local`** when only local connections are needed. This is
  the default and the most secure option.
- The HTTP control plane applies trust-boundary rules: read-only routes
  (agent state, events, tasks) still require a valid token for remote access.
- **Team deployments**: For shared multi-user deployments, switch to OIDC
  authentication (`auth.mode = "oidc"`). This replaces the shared control token
  with individual SSO logins. See [Configure OIDC authentication](/guides/configure-oidc-authentication.md).

## See Also

- [TUI reference](/reference/tui.md) — navigation, slash commands, and remote connect
- [HTTP control plane](/reference/http-control-plane.md) — API reference for programmatic access
- [Troubleshoot a Holon task](/guides/troubleshooting.md) — connection problems
- [Automate Holon over HTTP](/guides/automate-over-http.md) — drive Holon from code
- [Configure OIDC authentication](/guides/configure-oidc-authentication.md) — single sign-on and audit trails
