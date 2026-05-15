---
title: Remote access
summary: Remote daemon access — tunnel, tailnet, LAN modes, token management, and connecting from a remote TUI.
order: 22
---

# Remote Access

Holon supports running as a remote daemon and connecting from a TUI or API
client on another machine. This is the foundation for team-shared agents,
headless deployment, and remote development workflows.

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
  https://your-server:8787/v1/agents
```

See the [HTTP Control Plane reference](/reference/http-control-plane.md) for
the full API surface.

## Token Management

### Generating a Token

Tokens are generated when the server starts. Capture the token from the
startup output or create a dedicated file:

```bash
# On the server, save the token
holon daemon start --access tunnel --token-file ~/.holon/remote.token
```

### Token Profiles

Store multiple tokens as named profiles in your configuration:

```bash
holon config set tokens.office "token-for-office-server"
holon config set tokens.home "token-for-home-server"
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

- **Always use a token**. Remote connections without a token are rejected for
  non-local access modes.
- **Prefer tunnel or tailnet** over LAN mode when connecting across the
  internet. These provide encryption and authentication without exposing raw
  ports.
- **Rotate tokens** by restarting the daemon with a new `--token-file`.
- **Use `--access local`** when only local connections are needed. This is
  the default and the most secure option.
- The HTTP control plane applies trust-boundary rules: read-only routes
  (agent state, events, tasks) still require a valid token for remote access.

## See Also

- [TUI Guide](/guides/tui.md) — Full TUI navigation and slash command reference
- [HTTP Control Plane](/reference/http-control-plane.md) — API reference for programmatic access
- [Troubleshooting](/guides/troubleshooting.md) — TUI connection issues
- [Integration Guide](/guides/integration.md) — Programmatic integration patterns
