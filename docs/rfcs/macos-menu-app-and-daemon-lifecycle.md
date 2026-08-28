# macOS Menu App And Daemon Lifecycle

## Status

Accepted for implementation on August 28, 2026.

## Goal

Holon provides a native macOS menu bar control plane for users who should not
need a terminal to start, stop, inspect, or update the runtime. The existing
Rust daemon remains the single runtime. The menu app and command-line clients
must never create separate daemon identities or competing configuration and
lifecycle implementations.

The first supported system is macOS 13.

## Product Shape

`Holon.app` contains:

- a SwiftUI `MenuBarExtra` application;
- the matching Rust `holon` executable under `Contents/Resources/bin`;
- update metadata and, when release signing is configured, Sparkle;
- no launch agent or privileged helper.

The menu app is an accessory application without a default Dock icon. It may
open ordinary settings and diagnostics windows.

## Runtime Ownership

The MVP lifecycle owner is `standalone`. Both the bundled executable and an
external compatible CLI address the same daemon home, control socket, metadata,
and local control API.

The menu app does not:

- inspect PID or socket files to make lifecycle decisions;
- implement process cleanup or signal escalation in Swift;
- supervise the daemon as a child whose lifetime is tied to the app;
- register a `LaunchAgent`.

Instead it invokes the bundled `holon daemon` commands with structured
arguments and consumes their JSON results. Rust remains the only implementation
of daemon identity checks, stale-state cleanup, graceful shutdown, and process
fallback behavior.

## Lifecycle Contract

Every status or mutation result identifies:

- the lifecycle state;
- daemon product version and control protocol version when known;
- lifecycle owner;
- executable path when known;
- the canonical Web UI URL;
- runtime health and the most recent failure summary.

The public state model is:

- `starting`
- `running`
- `stopping`
- `stopped`
- `stale`
- `degraded`
- `version_mismatch`

Rust may complete a short transition before returning a mutation response, but
clients must not infer success optimistically. A mutation returns a final
snapshot or a structured error.

Cross-process lifecycle mutations are serialized. `start` remains idempotent.
Conflicting `stop`, `restart`, and update operations cannot interleave.

## Compatibility

Control protocol versions are independent from product versions. A client may
control a daemon when their protocol versions are compatible even when product
versions differ.

An incompatible client:

- may display status and diagnostics that can be read safely;
- must not blindly kill or replace the daemon;
- must show the actual daemon executable and product version;
- directs the user to update or explicitly restart with a compatible version.

Additive JSON fields remain backward compatible. Readers must tolerate unknown
fields, and new optional fields must deserialize from older responses.

## Login And Desired State

`SMAppService.mainApp` controls only **Launch Holon Menu App at Login**.

Daemon desired state records the user's last explicit lifecycle decision:

- explicit Start sets `desired_running = true`;
- explicit Stop sets `desired_running = false`;
- Restart preserves `true`;
- logout, shutdown, app exit, or an unexpected daemon failure does not rewrite
  the desired state.

When the menu app starts, it reads this shared desired state. It starts a
stopped daemon only when `desired_running` is true. The CLI and menu app update
the same state through the Rust lifecycle contract.

Quitting the menu app does not stop Holon.

## Updates

The supported release transaction replaces the complete app bundle:

1. record whether the daemon should be running after the update;
2. request graceful daemon shutdown;
3. install the signed and notarized app update;
4. launch the new app and run compatible migrations;
5. restore the recorded desired state.

The menu app does not update or overwrite an arbitrary external CLI. The
initial command-line installation action creates a user-owned link or shim
under `~/.local/bin` only after checking for an existing command. It never
silently replaces Homebrew, Cargo, or administrator-owned installations.

## Release Boundary

The initial distribution channel is a Developer ID signed and notarized DMG.
Mac App Store sandboxing and launch-agent supervision are outside this phase.
Release automation must verify nested signatures, notarization, stapling, and
the bundled CLI version before publishing an update feed.

## Preserved Boundaries

- The daemon is the runtime and configuration fact source.
- Rust owns lifecycle behavior; Swift owns presentation and OS integration.
- CLI and GUI remain independently usable clients.
- Login launch, daemon desired state, quitting the app, and uninstalling are
  distinct user actions.
- A future launchd owner requires a separate lifecycle ownership decision and
  is not implied by this contract.
