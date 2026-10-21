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

## Branding And Localization

The app bundle declares a multi-resolution `Holon.icns`, generated from the
existing Holon brand mark during packaging. This is separate from the template
image used by the menu bar, and does not change the accessory/Dock lifecycle.

User-facing app text supports English and Simplified Chinese, selected through
macOS language preferences with English as the development fallback. SwiftPM
owns the localized resource bundle; the outer app declares the same supported
languages and packaging verification checks that both catalogs are present.
CLI commands, paths, URLs and daemon configuration values remain untranslated.
Additional languages can extend the same catalogs without changing the runtime
or introducing a separate language setting.

## Product Shape

`Holon.app` contains:

- an AppKit menu bar application with SwiftUI content in an `NSPopover`;
- the matching Rust `holon` executable under `Contents/Resources/bin`;
- update metadata and, when release signing is configured, Sparkle;
- no launch agent or privileged helper.

The menu app is an accessory application without a default Dock icon. AppKit
owns startup and the `NSStatusItem`; SwiftUI only renders the popover content.
The popover measures the content's natural height but caps its viewport to the
visible area below the status button on that button's screen, leaving room for
the popover border and arrow. Overflow scrolls vertically instead of clipping
the menu. Opening the popover rechecks the screen, and content changes (such as
pairing QR expansion) resize the viewport within the same cap.
There is no placeholder settings scene or default window. Reopening the app
does not create a window, and closing the last window does not quit the menu
app. Any future settings or diagnostics window must be added explicitly with
real content.

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

### Standalone file logging

The standalone `serve` process, not the short-lived launcher or menu app, owns
its log collector. On Unix the launcher marks this managed child; before
tracing or Tokio starts, the child captures both stdout and stderr through a
bounded local socket into one size-rotating writer. Foreground `serve` and
service-manager/journald output are not redirected by default. Unsupported
platforms fail managed capture explicitly rather than silently using unbounded
append. Tracing enables ANSI only when stderr is a terminal; arbitrary native
output is not post-processed to remove control sequences.

The fixed first-version retention is four segments of at most 16 MiB each:
`daemon.log`, then `.1` through `.3` (oldest). Large writes are split at the
boundary. A process-lifetime file lock excludes competing managed writers.
On startup, oversized legacy segments retain only their latest 16 MiB, using
bounded copying; no giant historical backup is kept. This retention discards
old history and reports the discarded byte count. Migration may temporarily
need one additional 16 MiB suffix file; it never reads a whole legacy log into
memory. Only explicitly named managed segments and metadata are touched.

Initialization and capture failures fail startup with a bootstrap diagnostic.
At runtime an I/O failure never falls back to unbounded append or recursively
logs through the failed destination. The collector keeps draining, retaining
at most 64 KiB of pending bytes and counting overflow as dropped bytes. A
one-second read timeout also retries and publishes loss after a burst followed
by silence; continuous output observes the same one-second retry backoff.
On recovery buffered bytes are written before new bytes; shutdown makes one
final retry and accounts for any remaining discarded bytes. Logging failure does not automatically kill
the daemon or change runtime readiness.

`daemon logs` exposes additive `log_health` diagnostics (degradation, buffered
and dropped bytes, historical retention and last error). A fixed 4 KiB
preallocated `daemon.log.status.json` block avoids unbounded failure reports
and permits ordinary disk-full diagnostics without new allocations on disk.
If the filesystem itself cannot update this block, persisted diagnostics may
be stale; this is not a guarantee of observability on a completely failed
filesystem. These diagnostics are separate from runtime failure summaries.
The tail reader spans the four segments with one shared 128 KiB read budget
and the existing 8 Ki-character per-line limit, including during rotation.

Orderly exit flushes standard streams, fences inherited socket write ends,
drains and joins the in-process collector; it leaves no logging helper process.
Panic unwinding normally also drains via the guard. Process abort, SIGKILL,
power loss and writes from descendants racing shutdown can lose the final
in-flight bytes; crash exactly-once delivery is not promised.

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

The lifecycle client treats a daemon as incompatible only when its control
protocol version is incompatible. Product build identity remains diagnostic
metadata and does not prevent lifecycle control.

An incompatible client:

- may display status and diagnostics that can be read safely;
- must not blindly kill or replace the daemon;
- must show the actual daemon executable and product version;
- may replace it during restart only after Rust verifies that the metadata, PID
  file, configured home and socket, and live process executable all identify
  the same Holon daemon.

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

When the menu app starts, it reads this shared desired state. When
`desired_running` is true, it starts a stopped daemon or safely restarts a
control-protocol-incompatible recorded Holon daemon through the Rust lifecycle
contract. A different product version with a compatible control protocol is
adopted without restart. The CLI and menu app update the same state through that
contract.

Quitting the menu app does not stop Holon.

### Persistent serve intent

The daemon owns `daemon/serve_profile.json`, separately from the
`desired_running` boolean and ephemeral runtime metadata. See
[Daemon Serve Profile Persistence](daemon-serve-profile-persistence.md).
Ordinary Start, Restart, login recovery and update recovery inherit this intent;
the menu does not derive listener settings from `http_addr` or inject desktop
and authentication defaults. For a first startup it sends the request-only
`--desktop-integration-if-unconfigured` hint; the daemon confirms the absence
of profile and legacy intent under its lifecycle lock before enabling desktop
integration. A status snapshot is not sufficient: intent accepted between
Status and Start / Restart must be inherited, not overwritten. Explicit network
or credential changes submit a patch through the Rust lifecycle contract.

This profile does not grant process ownership, credential approval or a session.
CLI and explicit environment overrides remain higher priority and are reported
in startup diagnostics. Old binaries that do not read the profile are outside
the inheritance guarantee.

## Network controls

LAN, Tailscale Serve, and Finder opt-in are independent. The menu's LAN preset
uses an explicit IPv4 wildcard listener and a separate LAN advertised host, so
its loopback control requests and Serve backend remain reachable. This exposes
all IPv4 interfaces, not just one LAN adapter; confirmation and effective TCP
authentication are required. Disabling LAN restores loopback without disabling
Serve or removing an approved credential. Custom/tailnet-only listeners are not
silently converted to this LAN preset.

An explicit enable-Serve action may prepare a validated private token for a
menu-managed daemon and restart it to load authentication, while retaining
listener, port, access and desktop opt-in. Unknown/external credentials must not
be overwritten. Matching executable paths alone do not establish menu ownership;
without known menu launch provenance, authentication preparation must instead
provide explicit restart/configuration guidance, not automatically take over
an existing daemon. Lifecycle command JSON includes `process_created`: Start
reports false when reusing an existing daemon and true after spawning one;
Restart forwards its start outcome, and Stop/PrepareUpdate report false.
The menu records Start ownership only for explicit true; an absent field from
older binaries is unknown, not evidence of ownership.
Authentication preparation additionally requires same-client-instance creation
provenance and a still-matching healthy PID, home, socket, executable, listener,
and configuration fingerprint. Persisted PID records do not authorize it.
Disabled authentication with an existing credential is rejected. Preparation
restarts with only the private token file, omitting listen, access, advertise,
and desktop flags to inherit configuration; explicitly repeating `--listen`
would clear the inherited advertised URL under the CLI override contract.
The restarted listener must match the original, and authenticated status must
be rechecked before the enable POST.
Start/restart retains approved remote credentials; a local first
start does not generate one merely to run locally. Independent status failures
must not erase other known network/lifecycle facts. See
[pairing and Serve control](device-pairing-and-tailscale-serve-control.md) for
destination selection, ticket invalidation, and desired-versus-actual state.

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
