---
title: Connect with the Android client
summary: Set up the native Android client, pair with a running Holon daemon, and manage agents on mobile.
order: 37
---

# Connect with the Android Client

Holon includes a native Android client designed for mobile workflows. The app provides a focused conversation experience with completion briefs, local history caching, and quick pairing with running daemons.

This guide covers installation, connection setup, network profiles, and mobile features.

## Prerequisites

- A running Holon daemon with HTTP enabled (default port `7878`).
- An Android device running Android 8.0 (API level 26) or newer, or an emulator.
- For USB debugging: `adb` installed on your development machine.

## Installation

### Download Official Releases

Download official signed packages directly from the [Holon GitHub Releases](https://github.com/holon-run/holon/releases) page. Each release provides `holon-android-v<version>.apk` for direct device installation and `holon-android-v<version>.aab` for Google Play distribution.

Install the APK directly on your device:

```bash
adb install holon-android-v0.48.0.apk
```

### Build from Source

To compile the Android app locally:

```bash
cd apps/android
./gradlew :app:assembleDebug
adb install app/build/outputs/apk/debug/app-debug.apk
```

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

1. Open the Holon app.
2. Enter the **API Base URL** (e.g. `http://10.0.2.2:7878/api` or `https://<your-host>/api`).
3. Enter your bearer token or bootstrap secret.
4. Tap **Connect**.

The app calls `/api/auth/session/exchange/native`, stores the resulting session credential in Android Keystore, and clears the input token from memory.

### Option C: Sign in with OIDC (Single Sign-On)

If your daemon runs with OIDC authentication enabled:

1. Open the Holon app and enter the **API Base URL**.
2. Tap **Sign in with SSO**.
3. The app opens your system browser to complete identity provider sign-in.
4. The browser redirects back to Holon via `run.holon.android://oidc/callback`.

The app exchanges the one-time bootstrap ticket for a revocable session credential, encrypts it in Android Keystore for the current profile, and connects immediately.

## Managing Network Profiles

The app supports saving multiple network profiles in **Settings** (such as *Home Tailscale*, *Office LAN*, and *USB Localhost*). Once saved, you can switch environments with a single tap without re-entering credentials.

## Mobile Features

- **Conversation-First Reading & Local Cache:** Browse active agent threads with instant loading. Conversation history caches locally on the device, allowing you to review past turns and deliverables even with intermittent connectivity.
- **Daily Navigation & Task Previews:** Jump across daily activity streams, inspect active background task status cards, and view live child-agent previews directly from the mobile timeline.
- **Brief-First Summaries:** Prominently highlights completion briefs, active work items, and checklist progress so you can track outcomes without scrolling through full execution traces.
- **Model Selection:** View and override the active model for any agent directly from the mobile interface.
- **System Share Sheet Integration:** Share text, links, or documents from other Android apps directly into an agent's input composer.
- **Durable Outbox:** Prompts written while offline queue locally in the outbox and dispatch automatically when connection resumes.
- **Diagnostic Traces:** Export a redacted ring-buffer diagnostic log from Settings to troubleshoot connectivity or synchronization issues.

## Language Settings

The app interface supports English and Simplified Chinese (`简体中文`). It follows your system locale by default, but you can select a specific language in the login screen or under **Settings**.

## Next Steps

- [Remote access guide](/guides/connect-remote-runtime.md) — Secure network configuration for remote daemons.
- [OIDC authentication guide](/guides/configure-oidc-authentication.md) — Centralized authentication and session policies.
- [HTTP control plane reference](/reference/http-control-plane.md) — Endpoints used by the Android SDK.
