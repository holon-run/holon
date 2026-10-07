---
title: Holon Android Privacy Policy
summary: Data handling in the Holon Android client.
order: 100
---

# Holon Android Privacy Policy

Updated: October 7, 2026

[中文](/zh-CN/privacy)

Holon Android is developed by jolestar. Contact: **hello@holon.run**.
This policy covers the Android client, which connects to a Holon host you
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
token is not written to disk. Native session credentials and browser-login proof
material are encrypted using Android Keystore-backed storage. Browser sign-in
uses the host's OIDC flow and identity provider, which process login information.
The app also stores connection profiles, preferences, cached conversation and
brief data, and pending outgoing messages locally.

## Files, camera and diagnostics

Files and images are selected through Android's document picker. Taking a photo
opens a camera app; connection QR scanning uses Google Play services' code
scanner. The app manifest requests internet and network-state access, not
camera, microphone, contacts or location permissions. Camera capture and QR
scanning still involve camera use through those separate system/provider flows.
Google Play services may have its own data handling; see your device's Google
privacy information.

The client records local connection diagnostics and redacted traces. You can
choose to export diagnostics through Android's share sheet; inspect exports
before sharing. Holon Android has no ads. No advertising or analytics SDK is
declared in the Android app's build dependencies. This is not a claim that
your host, browser, Google Play services, or external providers collect no data.

## Retention and deletion

Local storage can be removed through Android's **Clear storage** action or by
uninstalling the app. Android application backup is disabled. Removing local
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
