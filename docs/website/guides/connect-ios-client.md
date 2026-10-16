---
title: Connect with the iOS client
summary: Set up the native iOS client, pair with a running Holon daemon, inspect deliverables and files, and report AI content on iPhone and iPad.
order: 200
---

# Connect with the iOS Client

Connect an iPhone or iPad to a Holon runtime, send a request, and open the
result or its files. Previously cached conversations remain useful offline,
but they do not confirm that the runtime is reachable.

## Prerequisites

- A running Holon daemon with HTTP access enabled (default port `7878`).
- An iPhone or iPad running iOS 18.0 or newer, or a compatible Xcode simulator.
- A network path to the daemon (LAN, reverse proxy, or tailnet).
- A pairing invitation, a control token, or organization sign-in.

## Installation

### Install Official Packages

If you have a TestFlight invitation, follow its redemption link on your device.
For other distribution channels, check the project's announced availability;
this guide does not imply that an App Store build is available.

### Build from Source

The Xcode build and run steps have moved to
[Build mobile clients](../maintainers/build-mobile-clients.md#ios).

## Connecting to the Daemon

### Option A: QR Code Pairing (Recommended)

1. In the Holon Web GUI, open **Settings** -> **Device Pairing** (or choose **Pair Device** in the macOS status menu).
2. Open the Holon app on your iOS device.
3. Tap **Scan connection QR code** on the onboarding screen.
4. Point your camera at the QR code displayed in the Web GUI or terminal.

The app reads the invitation ticket (`/login#pair=<ticket>`), redeems it against `POST /api/auth/pairing/redeem/native`, securely saves the session credential in the iOS Keychain, and navigates directly to your agents.

If your camera is unavailable or permission is denied, tap the manual entry option and paste the pairing link into the ticket field.

### Option B: Manual Connection

1. Tap **Manual Connection** on the onboarding screen.
2. Enter the full API base URL (e.g. `https://holon.example.com/api` or `http://192.168.1.50:7878/api`).
3. Enter your bearer token or bootstrap secret.
4. Review the data-sharing disclosure and tap **Connect**.

The app calls `POST /api/auth/session/exchange/native`, stores the revocable session credential in the Keychain, and clears the raw token from memory.

### Option C: Organization Single Sign-On (OIDC)

When your daemon connects to an OpenID Connect provider:

1. Enter the API base URL.
2. Tap **Sign in with Organization**.
3. Complete authentication in the system browser sheet.
4. The browser returns control to Holon via custom URL callback, and the app stores the issued session token in the Keychain.

## Network and Security Considerations

- **Prefer HTTPS:** Production deployments should always use HTTPS. Connecting over unencrypted HTTP triggers an explicit confirmation dialog before redemption or login.
- **Local Network Permissions:** Connecting across a home or office Wi-Fi network requires granting the iOS Local Network permission when prompted.
- **Loopback Addresses:** On a physical phone, `127.0.0.1` and `localhost` refer to the phone itself. Use your computer's LAN IP or a Tailscale MagicDNS address to reach the daemon.

## Daily Navigation and Mobile Experience

After connecting, select an agent and send a harmless request:

```text
Reply with "iOS connection verified". Do not change any files.
```

Wait for a new completion brief. Check that the request was sent to the selected
runtime rather than left in a local sending queue.

- **Searchable Agent Roster:** The main screen lists accessible agents with real-time status and activity summaries. Filter agents by recent activity, reply state, or name.
- **Result-First Timeline:** Conversations display operator requests alongside final Markdown completion briefs. Detailed execution traces and tool outputs collapse into expandable turn rows so you can review deliverables without scrolling through internal logs.
- **Background Task Visibility:** Ongoing background runs and child-agent invocations appear as compact status cards that reflect current execution state.
- **Model Selection & Stopping:** Inspect or override the active model route directly from the conversation composer. You can also stop an active run with one tap.

## Native File and Artifact Workflows

Tap the folder or action icon inside an agent conversation to view its attached workspace and generated artifacts:

- **Paged Reading for Large Files:** Files open in a dedicated native reader that uses paged disk chunks, so large Markdown reports, diffs, or logs do not degrade scrolling performance.
- **Search and Syntax Highlighting:** Search text matches in real time and inspect syntax-highlighted source code across common languages.
- **Media and Document Previews:** Pinch to zoom generated images, and preview PDF files with passive page navigation.
- **Verbatim Text Selection:** Long-press any rich text block and select **Select source text** to copy unformatted source ranges directly to your clipboard.

## System Sharing and Share Inbox

- **Share Sheet Extension:** Select text, web URLs, or photos in Safari or Files, open the iOS Share Sheet, pick Holon, choose a destination agent, and tap **Send**.
- **Offline Staging:** If your device is offline when sharing, items save to **Settings** -> **Share inbox** so you can review or dispatch them once connected.

## Report AI Content

You can report inaccurate, harmful, or policy-violating assistant responses directly from the app:

1. Long-press the relevant assistant message or its turn container.
2. Choose **Report AI content** from the context menu.
3. Select an applicable reason category (such as harmful, inaccurate, or copyright violation).
4. Optionally provide details in the text box.
5. Tap **Submit Report**.

The client submits the report to `POST /api/content-reports`. The server stores
an authenticated, bounded snapshot of the selected response for administrator
review, not the entire conversation. Review the selected text: it may itself
contain sensitive information. Acceptance by the runtime does not mean the
report has been moderated.

## Managing Connections and Diagnostics

- **Switching Connections:** In **Settings** -> **Connections**, view configured endpoints or swipe left to delete unused servers.
- **Diagnostic Reports:** In **Settings** -> **Diagnostics**, generate a sanitized report containing connection state, system version, and event counters to share when troubleshooting issues.

## Verify results and file access

1. Check the saved connection address and confirm that the expected agent
   received your test request and returned a new brief.
2. Open **Files** from that agent's conversation. Select a known, harmless text
   file and verify its contents and workspace.
3. Open a file reference from a brief, if available. Confirm that it resolves to
   the intended runtime artifact, especially when the agent uses a worktree.
4. Reopen the app and refresh. Cached conversation text is available within
   the app's cache limits; execution details and files are not promised as
   offline downloads.

## Troubleshooting

- **Cannot connect on Wi-Fi:** Check iOS Local Network permission, the server
  address, and the host's listen interface. A physical phone cannot reach your
  computer through the phone's `localhost`.
- **Pairing fails:** Request a fresh invitation. Check its host before redeeming
  it and ensure the phone can reach that host.
- **Organization login returns without a session:** Retry sign-in and check
  the runtime's OIDC configuration. Do not substitute a shared local token.
- **Conversation opens but files do not:** Restore the live connection. Check
  that the referenced workspace or execution root still exists and is
  accessible; cached briefs do not cache all linked files.
- **Request remains queued:** Reconnect and inspect the sending queue before
  resubmitting, to avoid duplicate requests.
- **Persistent failure:** Generate a diagnostic report in Settings, review it
  before sharing, and include the failing step and client/runtime versions.
  Do not include credentials or pairing invitations.

## Next Steps

- [Connect to a remote Holon runtime](/guides/connect-remote-runtime.md) — Secure network setup with Tailscale and reverse proxies.
- [Configure OIDC authentication](/guides/configure-oidc-authentication.md) — Set up SSO and token policies.
- [Connect with the Android client](/guides/connect-android-client.md) — Set up the native Android client.
