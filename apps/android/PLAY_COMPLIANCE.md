# Play compliance evidence and release gates

This is a source-based preparation record, not a legal compliance guarantee or
a completed Play Console declaration. Developer: jolestar; support:
hello@holon.run. The app is not directed at children.

## Privacy policy

The proposed public URLs are `https://holon.run/privacy` and
`https://holon.run/zh-CN/privacy`, following the docs site's Markdown routes.
**Not deployed by this change.** Verify both are public, accessible without
authentication and render correctly before submitting them to Play Console.
Settings → About links to the policy in the selected UI language. The website
footer also provides an entry. Publishing the website remains a release gate.

## Source evidence

Paths below are relative to `apps/android/`.

| Topic | Evidence |
| --- | --- |
| Host selection and HTTP consent | `app/src/main/kotlin/run/holon/android/app/ConnectionScreens.kt`, `sdk/src/main/kotlin/run/holon/android/sdk/HolonHttpClient.kt` |
| Release default address | `app/src/main/kotlin/run/holon/android/app/DefaultBaseUrl.kt` pre-fills `https://holon.run/api`; confirm whether an actual developer-operated service is offered there |
| Native sessions; encrypted credentials and login proof | `app/src/main/kotlin/run/holon/android/app/SessionStore.kt`, `HolonViewModel.kt` in the same directory |
| OIDC browser flow; state, host and start time stored locally | `app/src/main/kotlin/run/holon/android/app/HolonViewModel.kt` |
| Messages and base64 attachments sent to host | `sdk/src/main/kotlin/run/holon/android/sdk/HolonHttpClient.kt` |
| Document picker and delegated camera capture | `app/src/main/kotlin/run/holon/android/app/ConversationScreen.kt` |
| QR scan via Play services | `app/src/main/kotlin/run/holon/android/app/ConnectionScreens.kt`, `app/build.gradle.kts` |
| Network permissions, backup disabled | `app/src/main/AndroidManifest.xml` |
| Local Room cache/outbox and DataStore profiles | `app/src/main/kotlin/run/holon/android/app/HolonStorage.kt` |
| Local traces; user-initiated diagnostic sharing | `app/src/main/kotlin/run/holon/android/app/AndroidTrace.kt`, `SettingsScreen.kt` in the same directory |
| No ad/analytics SDK declared in app build | `app/build.gradle.kts` (not a guarantee about transitively bundled SDKs or host processing) |

## Proposed Console preparation — operator confirmation required

- **Audience:** not child-directed is confirmed. Choose actual intended age
  bands rather than inferring them from that statement; do not select Families
  participation without a separate review.
- **Ads:** the operator confirms Holon Android has no ads. No ad SDK or ad
  placement was found in the app sources examined. Prepare the Console answer
  as **No**; still verify the final distributed build and SDK inventory.
- **Data safety:** do not answer “no collection” merely because hosts are
  self-hosted or code is open source. Inventory message text, attachments,
  identifiers/authentication, diagnostics and provider/SDK processing against
  the actual distributed app and backend arrangements. Collection vs sharing,
  optional vs required, purpose, retention and deletion answers require those
  operational facts. A blanket “all data encrypted in transit” claim conflicts
  with supported HTTP connections.
- **Content rating:** complete the questionnaire for actual capabilities,
  user content and AI-generated responses; do not assign a rating from source
  alone. Confirm moderation and access to externally provided content.
- **Review access:** prepare a reviewer-accessible test host, test account or
  credentials and precise connection/login instructions. No credentials belong
  in this repository. Do not assume a reviewer can reach a LAN/tailnet host.
- **AI content:** agent conversations can return AI-generated content. No
  dedicated in-app report/flag flow for offensive AI content was identified in
  the reviewed Android conversation/settings UI. Treat applicability and the
  reporting requirement as an unresolved release gate; this task does not add
  a reporting backend or broaden the UI implementation.
  Self-hosting or remote model execution is not treated as an automatic policy
  exemption. A support email or a referral to the host administrator is not
  represented as an implemented in-app AI-content report flow.

Holon is a client for self-hosted agents, not a single developer-operated
chat/model service. The host operator (the user for a personal deployment)
controls models, integrations, server records, logs and backups. The client
developer handles software and privacy questions, but cannot inspect or delete
private-host content. Do not promise centralized moderation, a fixed model
provider, universal retention, or a reporting endpoint that does not exist.
Resolve AI-policy applicability against actual functionality and, if needed,
the reporting/safety-improvement process before public release; this record
does not claim a policy exemption or completed moderation capability.

Confirm who operates any supplied/default or reviewer host, what providers it
uses, what it logs, who can access those logs, and its retention/deletion
procedures. Also verify Google Play services code-scanner data disclosures for
the shipped version, the full SDK inventory, account-creation/deletion policy
applicability, and whether an external deletion-request URL is needed.
Do not invent a retention period or equate clearing app storage with deleting
server accounts or revoking sessions.

## Store materials and confirmed contact

Use developer name **jolestar** and public support email **hello@holon.run**.
These are confirmed source values, not evidence that the corresponding Console
fields have been saved. The non-child-directed audience is confirmed; exact
age bands and the final declarations still need the account holder's decision.

[Store and article assets](play/README.md) contain English captures of a real
client with persisted, clearly identified demo conversations, plus launcher
icon and feature-graphic sources. They are not visual-test snapshots and have
not been uploaded. The loopback screenshot service is not reachable by Play
reviewers; a public reviewer host/account needs separate authorization and
operational facts before it can be offered.

Official policy references (review at submission time):

- User data: <https://support.google.com/googleplay/android-developer/answer/10144311>
- Data safety: <https://support.google.com/googleplay/android-developer/answer/10787469>
- AI-generated content: <https://support.google.com/googleplay/android-developer/answer/13985936>
- AI policy guidance: <https://support.google.com/googleplay/android-developer/answer/14094294>
