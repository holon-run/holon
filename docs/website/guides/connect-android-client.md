---
title: Connect with the Android client
summary: Set up the native Android client, connect to a running Holon daemon, and manage agents from mobile.
order: 37
---

# Connect with the Android Client

Holon provides a native Android client powered by the Holon Android SDK.
Designed for mobile daily use, the app delivers a brief-first agent workspace
to review high-signal outcomes, inspect workspace files, and dispatch tasks
from an Android phone, tablet, or emulator.

This guide explains how to connect the Android app to a running Holon daemon.

## Prerequisites

- A running Holon daemon with the HTTP control plane enabled (default port `7878`).
- An Android device running Android 8.0 (API level 26) or newer, or an Android emulator.
- For local USB testing: `adb` installed on your development workstation.

## Connection Options

Choose the network setup that matches your environment:

| Target Environment | Default API Base URL | Host Preparation |
|---------------------|----------------------|------------------|
| Android Emulator | `http://10.0.2.2:7878/api` | None required |
| Physical Device (USB) | `http://127.0.0.1:7878/api` | `adb reverse tcp:7878 tcp:7878` |
| Local LAN / Remote | `https://holon.example.com/api` | Reverse proxy or tailnet |

> **Security Note:** Production deployments should always use HTTPS. While the
> debug build allows loopback HTTP without warning, connecting over unencrypted
> HTTP across a network requires explicit confirmation in the app.

## Step 1: Prepare the Daemon

Ensure your daemon listens on the network interface you intend to reach. For
USB or emulator access, localhost is sufficient:

```bash
holon daemon start
```

If your daemon requires authentication, have your access token ready. This can
be the bearer token configured when starting the daemon (`--token <TOKEN>` or
`--token-file <PATH>`) or a bootstrap token generated during setup.

## Step 2: Configure Port Forwarding for USB Devices

If you are testing on a physical phone connected over USB, route device traffic
to your workstation daemon:

```bash
adb reverse tcp:7878 tcp:7878
```

You do not need this step when using the standard Android emulator.

## Step 3: Log In from the Android App

1. Open the Holon app on your device.
2. Enter your **API Base URL**:
   - For the emulator: `http://10.0.2.2:7878/api`
   - For a USB-forwarded phone: `http://127.0.0.1:7878/api`
   - For a remote server: `https://<your-host>/api`
3. Enter your auth token or bootstrap secret.
4. Tap **Connect**.

The app exchanges your token via the daemon's `/api/auth/session/exchange/native`
endpoint, creates an encrypted session stored securely in Android Keystore, and
discards the input token from memory.

## Step 4: Interact with Agents

Once connected, the mobile workspace provides:

- **Brief-First Workspace:** Focus on high-signal outcomes. The app elevates
  completion briefs, active work items, and checklist progress, giving you an
  immediate read on deliverables without wading through internal execution traces.
- **Live Roster Sync:** Browse all persistent agents, current posture (Awake or
  Sleeping), and active children. The roster updates automatically through live
  runtime events without pulling to refresh.
- **File Reader and Message Links:** Read plan documents, Markdown notes, and
  workspace files in an integrated reader. When an agent references a workspace file
  path in conversation, tap the link to open the file directly on your device.
- **Composer with Durable Outbox:** Submit tasks with a responsive multi-line
  input composer. If connectivity drops, prompts queue locally in a durable outbox
  and transmit automatically once the connection restores.

## Language Settings

The Android app interface supports both **English** and **Simplified Chinese**
(`简体中文`).

By default, the interface matches your device or per-app system language. You can
override this preference on the login screen or in **Settings** by choosing
*System default*, *English*, or *简体中文*. The selection remains saved on your
device across sign-outs.

## Build from Source

If you want to compile the Android app from the repository:

```bash
cd apps/android
./gradlew :app:assembleDebug
```

Install the resulting APK with `adb install app/build/outputs/apk/debug/app-debug.apk`.

## Next Steps

- [Remote access guide](/guides/connect-remote-runtime.md) — Secure network configuration for remote daemons.
- [OIDC authentication guide](/guides/configure-oidc-authentication.md) — Centralized authentication and session policies.
- [HTTP control plane reference](/reference/http-control-plane.md) — Endpoints used by the Android SDK.
