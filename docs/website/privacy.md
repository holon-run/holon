---
title: Holon Mobile Privacy Policy
summary: Data handling in the Holon Android and iOS clients.
order: 100
---

# Holon Mobile Privacy Policy

Updated: October 9, 2026

[中文](/zh-CN/privacy)

Holon Android and iOS are developed by jolestar. Contact: **hello@holon.run**.
This policy covers the Android and iOS clients, which connect to a Holon host you
choose. It does not replace the policies of your host operator, identity
provider, or model and integration providers configured on that host.

## Self-hosted services and responsibilities

The app is a client, not a centrally hosted AI service. You may operate your
own Holon host or use a host with its operator's authorization. If you deploy
the host yourself, you are its operator. That operator chooses the identity
provider, AI models, integrations, access controls and server-side data rules.
There is no single model provider used by every installation.

Installing this client does not give its developer access to conversations
on your private host or permission to inspect or delete that host's records.
For host-side access, content or deletion requests, contact the host operator.
For client software or privacy questions, contact
**hello@holon.run**. Do not send credentials or private conversations by email.
This support address is not an in-app AI-content reporting service.

## Connections and information you provide

The app sends your messages, selected attachments, and requested actions to
your selected host, and retrieves agent messages, briefs, tasks and files.
The host receives network connection information, including your IP address.
Your host may pass content to its configured AI or integration providers.
Choose a host and providers you trust; their processing and retention depend
on their configuration and policies.

Personal access tokens are exchanged for revocable native sessions; the original
token is not written to disk. On Android, native session credentials and browser-login proof
material are encrypted using Android Keystore-backed storage. Browser sign-in
uses the host's OIDC flow and identity provider, which process login information.
The app also stores connection profiles, preferences, cached conversation and
brief data, and pending outgoing messages locally.

On iOS, session credentials and pending browser-login proof material are stored
in Keychain; connection profiles and preferences use UserDefaults. Pending
messages and attachments are stored locally. The app and its share extension
use an App Group container for imported content and sharing state, and a shared
Keychain access group for the session used to send from the extension. These
are local app/extension storage mechanisms, not a central developer database.

## Files, camera and diagnostics

On Android, files and images are selected through the document picker. Taking a photo
opens a camera app; connection QR scanning uses Google Play services' code
scanner. The app manifest requests internet and network-state access, not
camera, microphone, contacts or location permissions. Camera capture and QR
scanning still involve camera use through those separate system/provider flows.
Google Play services may have its own data handling; see your device's Google
privacy information.

On iOS, system file and photo pickers let you select attachments. Taking a photo
and scanning a connection QR code use the camera with your permission. QR
scanning reads connection information locally; attaching a captured photo sends
it to the selected host when you send the message. The share extension can
receive text, URLs, images and files from other apps, copy imported content to
the App Group container, and send it to a selected agent on your host or prepare
it for confirmation in the app. Sharing or exporting content through system
share sheets makes it available to the destination you choose; that
destination's data handling is outside this client's control.

The Android client records local connection diagnostics and redacted traces. You can
choose to export diagnostics through Android's share sheet; inspect exports
before sharing. Holon Android has no ads. No advertising or analytics SDK is
declared in the Android app's build dependencies. This is not a claim that
your host, browser, Google Play services, or external providers collect no data.

## Retention and deletion

On Android, local storage can be removed through **Clear storage** or by
uninstalling the app. Android application backup is disabled.
On iOS, removing a connection profile removes its associated app session
credentials. Deleting the app removes its app-container data, but must not be
treated as a guarantee that Keychain items or shared App Group data are erased.
Local data may also be subject to your device's backup and restore settings.
Removing local
data does not delete host-side messages, files, accounts or provider records,
and does not necessarily revoke an existing server-side session. Ask your
host operator to delete server records or revoke access. If you operate the
host, manage its records, logs, backups and access yourself, and follow your
configured providers' deletion procedures for copies they hold. The operator
and each provider determine their own retention and deletion rules; the client
does not impose or promise a universal period.

## Security and audience

Use HTTPS for remote hosts. The client permits user-confirmed HTTP hosts for
local/self-hosted connections; HTTP does not provide transport encryption.
Only send information appropriate for your host and its configured providers.
The app is intended for adults aged 18 and over, not for children or teenagers
under 18. This audience statement does not imply that the client verifies age.
Questions about this policy or changes to it can be sent to **hello@holon.run**.
