# Remote TUI Access

Remote TUI access is an explicit control-plane mode for connecting `holon tui`
to a `holon serve` runtime on another host. The local Unix socket remains the
default trust path for same-host use.

## Terms

- `listen`: the address the server binds, such as `127.0.0.1:7878` or
  `0.0.0.0:7878`.
- `connect`: the URL a TUI client dials, such as `http://lab:7878`.
- `advertise`: the URL the server reports to clients as its reachable endpoint.
- `callback_base_url`: the URL external callback registrations should use.

`0.0.0.0` and `::` are valid only as listen addresses. They are not valid
connect, advertise, or callback URLs because clients cannot route to them.

## Server Modes

Default local mode is loopback only:

```sh
holon serve
holon serve --access local
```

SSH tunnel mode keeps the server loopback-bound and expects the operator to
forward a port explicitly:

```sh
holon serve --access tunnel --token-file ~/.config/holon/remote.token
ssh -L 7878:127.0.0.1:7878 lab
holon tui --connect http://127.0.0.1:7878 --token-file ~/.config/holon/remote.token
```

LAN and tailnet modes are explicit remote access modes and require bearer-token
authentication:

```sh
holon serve --access lan --host 192.168.1.10 --token-file ~/.config/holon/remote.token
holon daemon start --access lan --host 192.168.1.10 --token-file ~/.config/holon/remote.token
holon tui --connect http://192.168.1.10:7878 --token-file ~/.config/holon/remote.token

holon serve --access tailnet --host lab.tailnet.ts.net --token-file ~/.config/holon/remote.token
holon daemon start --access tailnet --host lab.tailnet.ts.net --token-file ~/.config/holon/remote.token
holon tui --connect http://lab.tailnet.ts.net:7878 --token-file ~/.config/holon/remote.token
```

`holon daemon restart` accepts the same access options as `daemon start` and
uses them for the replacement background `serve` process.

The lower-level form separates bind and client-visible URLs:

```sh
holon serve \
  --listen 0.0.0.0:7878 \
  --advertise http://lab.tailnet.ts.net:7878 \
  --token-file ~/.config/holon/remote.token
```

## Auth Contract

Any non-loopback TCP bind requires a configured control token. In token-required
TCP mode, all TUI-visible read, event, and write surfaces require bearer auth,
including agent lists, state snapshots, transcript/brief/task/timer views, SSE
event streams, public enqueue, and control actions.

Remote TUI mode must be explicit:

```sh
holon tui --connect http://host:7878 --token-file ~/.config/holon/remote.token
```

When `--connect` is present, the client never falls back to the local Unix
socket or local HTTP default, and it does not implicitly reuse local provider
credentials. The token must come from `--token`, `--token-file`, or
`--token-profile`.

## Handshake

Remote clients should call `GET /handshake` first. The response reports:

- control protocol name and version
- auth mode and whether bearer auth is required
- runtime capabilities
- default agent, workspace directory, home directory, listen address, and
  advertised URL when configured

Auth mismatch, unsupported URLs, missing tokens, and invalid advertise/connect
URLs should fail before silently falling back to local runtime state.
