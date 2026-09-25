---
title: Connect with the Android client
summary: Set up the native Android client, connect to a running Holon daemon, and manage agents from mobile.
order: 37
---

# Connect with the Android Client

Starting in v0.45.0, Holon includes a native Android client powered by an
Android SDK. The app lets you monitor agents, review conversation briefs,
and send tasks from an Android device or emulator.

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

If your daemon requires authentication, generate or copy a bearer token:

```bash
holon config get auth.token
```

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

The app exchanges your token with the daemon's session endpoint, creates an
encrypted session stored securely in Android Keystore, and discards the input
token from memory.

## Step 4: Interact with Agents

Once connected, you can:

- **Browse the Agent Roster:** View all persistent agents, current posture
  (Awake or Sleeping), and active children.
- **Track Work Items and Tasks:** Review active work items, checklists, and
  completion briefs.
- **Send Prompts:** Submit tasks directly into the agent's work queue.
  The app includes a durable outbox: prompts composed while offline or
  experiencing weak connectivity automatically queue and send once
  the connection recovers.
- **Inspect Artifacts:** View delivered summaries, markdown notes, and
  generated files right in the conversation timeline.

## Building from Source

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
