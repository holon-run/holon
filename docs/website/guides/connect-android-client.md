---
title: Connect with the Android client
summary: Set up the native Android client, pair with a running Holon daemon, and manage agents on mobile.
order: 190
---

# Connect with the Android Client

Connect your Android device to a Holon runtime, send an agent a request, and
read its result. The app keeps a local history cache, but you need a live
connection to confirm the runtime's current state.

## Prerequisites

- A running Holon daemon with HTTP enabled (default port `7878`).
- An Android device running Android 8.0 (API level 26) or newer, or an emulator.
- A network path to that daemon and either a pairing invitation, a control
  token, or organization sign-in.
- For USB installation or forwarding: `adb` installed on your computer.

## Installation

### Download Official Releases

Check the [Holon GitHub Releases](https://github.com/holon-run/holon/releases)
page for an Android APK attached to your chosen release. An `.apk` installs
directly; an `.aab` is a distribution bundle, not an installable APK.

Install the APK directly on your device:

```bash
adb install /path/to/downloaded-holon.apk
```

### Build from Source

The build and installation commands have moved to
[Build mobile clients](../maintainers/build-mobile-clients.md#android).

## Connection Options

| Target Environment | Default API Base URL | Host Setup |
|---------------------|----------------------|------------------|
| Tailscale / LAN | `https://<tailnet-host>/api` or `http://<lan-ip>:7878/api` | Enable Tailscale Serve or bind LAN interface |
| Android Emulator | `http://10.0.2.2:7878/api` | Default configuration |
| Physical Device (USB) | `http://127.0.0.1:7878/api` | `adb reverse tcp:7878 tcp:7878` |

> **Security Note:** Production setups should use HTTPS. Tailscale Serve provides automatic certificates for your tailnet. Connecting over unencrypted HTTP across a network prompts for explicit confirmation.

## Connecting to the Daemon

### Option A: QR Code Pairing (Recommended)

1. Open the Holon Web GUI in your browser and navigate to **Settings** -> **Device Pairing** (or select **Pair Device** in the macOS menu).
2. Open the Holon Android app and tap **Scan QR Code**.
3. Scan the one-time QR code.

The app exchanges the single-use ticket at `/api/auth/pairing/redeem/native` for a session credential, securely stores it in Android Keystore, and connects immediately.

### Option B: Manual Configuration

1. Open the Holon app. The server address starts empty; there is no default hosted server.
2. Enter your **Holon address** (e.g. `http://10.0.2.2:7878` or `https://<your-host>`), and confirm trusted-network use for HTTP.
3. Tap **Next**. The app checks the server's authentication method.
4. For a local-token server, enter your bearer token or bootstrap secret and tap **Sign in**.

The app calls `/api/auth/session/exchange/native`, stores the resulting session credential in Android Keystore, and clears the input token from memory.

### Option C: Sign in with OIDC (Single Sign-On)

If your daemon runs with OIDC authentication enabled:

1. Open the Holon app and enter your **Holon address**.
2. Tap **Next**. The app detects that the server uses OIDC.
3. The app opens your system browser to complete identity provider sign-in. If you return without signing in, **Use organization browser sign-in** lets you retry.
4. The browser redirects back to Holon via `run.holon.android://oidc/callback`.

The app exchanges the one-time bootstrap ticket for a revocable session credential, encrypts it in Android Keystore for the current profile, and connects immediately.

## Managing Network Profiles

The app supports saving multiple network profiles in **Settings** (such as *Home Tailscale*, *Office LAN*, and *USB Localhost*). Once saved, you can switch environments with a single tap without re-entering credentials. You can also delete unused network profiles directly from the settings list; deleting a profile immediately removes its configuration and associated Keystore credentials.

Deleting a profile also removes its local caches, drafts, unsent outbox items,
attachments, and diagnostics. Review pending work before deleting it; work
already submitted to the runtime is unaffected.

## Verify the connection and a result

1. Check the selected network profile and server address. Open an agent you
   recognize on that runtime.
2. Send a harmless request, such as `Reply with "Android connection verified".
   Do not change any files.`
3. Confirm that the message leaves the outbox and a new response arrives.
   Previously cached messages alone do not verify connectivity.
4. For tracked work, open the agent's Work view and distinguish the WorkItem's
   progress from individual Task status. Read the completion brief before
   deciding that the goal is finished.
5. Leave and reopen the app, then refresh the conversation to check that the
   saved profile reconnects.

## Mobile Features

- **Conversation-First Reading & Local Cache:** Browse active agent threads with instant loading. Conversation history caches locally on the device, allowing you to review past turns and deliverables even with intermittent connectivity.
- **Daily Navigation & Task Previews:** Jump across daily activity streams, inspect active background task status cards, and view live child-agent previews directly from the mobile timeline.
- **Brief-First Summaries:** Prominently highlights completion briefs, active work items, and checklist progress so you can track outcomes without scrolling through full execution traces.
- **Collapsed Background Activity:** In-progress and queued background messages collapse automatically, keeping the active conversation clean.
- **Model Selection:** View and override the active model for any agent directly from the mobile interface.
- **System Share Sheet Integration:** Share text, links, or documents from other Android apps directly into an agent's input composer.
- **Durable Outbox:** Prompts written while offline queue locally in the outbox and dispatch automatically when connection resumes.
- **Single-File Diagnostic Traces:** Export diagnostics saved to a single consolidated file that organizes events by network environment, simplifying troubleshooting and log sharing.
- **Report AI Content:** Long-press an assistant response to report inaccurate,
  harmful, or inappropriate content. The runtime stores a bounded snapshot of
  the selected response. Review its contents before reporting; a response can
  itself contain sensitive information.

## Language Settings

The app interface supports English and Simplified Chinese (`简体中文`). It follows your system locale by default, but you can select a specific language in the login screen or under **Settings**.

## Troubleshooting

- **Cannot reach the runtime:** Check the address from the phone's network.
  On a physical phone, `localhost` means the phone; use the host's LAN or
  tailnet address, or configure USB forwarding.
- **Pairing failed:** Generate a fresh single-use invitation and check its
  destination. A tailnet invitation requires the phone to join that tailnet
  and have access to the host.
- **Sign-in expired or denied:** Sign in again to the intended profile. Use
  organization sign-in for OIDC servers rather than entering a local token.
- **Old messages but no new response:** Check connection status and the
  outbox. Cached history does not prove the request reached the daemon.
- **Repeated connection failures:** Export the diagnostic trace from Settings.
  Review it for private data before sharing; include the client/runtime
  versions and the failing step, not tokens or pairing tickets.

## Next Steps

- [Remote access guide](/guides/connect-remote-runtime.md) — Secure network configuration for remote daemons.
- [OIDC authentication guide](/guides/configure-oidc-authentication.md) — Centralized authentication and session policies.
- [HTTP control plane reference](/reference/http-control-plane.md) — Endpoints used by the Android SDK.
