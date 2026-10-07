# Play compliance evidence and release gates

This is a source-based preparation record, not a legal compliance guarantee or
a completed Play Console declaration. Developer: jolestar; support:
hello@holon.run. The operator confirmed an adults-only (18+) target audience
on October 7, 2026. This does not determine the content rating.

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

## Console preparation — not submitted

- **Audience:** select **18 and over** only; do not select younger age bands
  or claim Families participation. This is the intended audience, not proof of
  age verification or an IARC content rating.
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
- **Review access:** mark the app as restricted by sign-in and supply the
  actual host, access token and instructions privately in Console.
  [Reviewer access preparation](PLAY_REVIEW_ACCESS.md) contains the host
  checklist and draft reviewer steps. **Not operational yet:** no public
  review host or credentials have been provisioned or submitted by this change.
- **AI content:** **treat the policy as applicable for release preparation**,
  based on the assessment below. This is not a Google review determination.
  The operator deferred the reporting closure; do not mark it complete or
  submit for public release as though this missing capability were exempt.

### AI-generated content assessment — October 7, 2026

Users can send new prompts to agents and receive newly generated responses.
Conversation is a central client feature, even though the model runs on the
selected remote host. Google lists central text-chat generation as in scope.
Holon is not limited to displaying existing AI content, summarization alone,
or AI assistance for an existing non-generative feature. Those limited-scope
examples do not justify an exemption for the current functionality.

The published policy does not establish a blanket self-hosted/remote-model
exemption. An 18+ audience also does not waive AI-content obligations.
Prepare against the applicable AI policy unless Google supplies a different
determination for the actual functionality; no such determination has been
obtained, and no policy-support request was sent in this task.

**Deferred release gate:** the reviewed Android UI has no dedicated in-app
offensive-content reporting/flagging flow. The policy requires an in-app path
to developers and use of reports to improve filtering/moderation, alongside
prevention of prohibited generation. A support email or referral to a private
host administrator is not an implemented substitute. Reporting closure and
evidence of effective safety controls remain unfinished. Deferral preserves
this gate; it is not a claim that an internal track is exempt.

Holon is a client for self-hosted agents, not a single developer-operated
chat/model service. The host operator (the user for a personal deployment)
controls models, integrations, server records, logs and backups. The client
developer handles software and privacy questions, but cannot inspect or delete
private-host content. Do not promise centralized moderation, a fixed model
provider, universal retention, or a reporting endpoint that does not exist.
Resolve the reporting/safety-improvement process before public release, or
obtain an explicit policy determination that changes the applicable requirements.
This record does not claim an exemption or completed moderation capability.

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
fields have been saved. The intended audience is confirmed as **18+ only**;
the final Console declarations and content-rating questionnaire are not saved.

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
- App content, sign-in details and target audience: <https://support.google.com/googleplay/android-developer/answer/9859455>
