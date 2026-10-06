# Holon iOS release preparation

This is a local preparation checklist, not approval to register identifiers,
sign for distribution, upload, or publish. The current client has not passed
physical-device, App Group signing or formal distribution acceptance.

## Reproducible version inputs

Both `Info.plist` and `ShareExtension/Info.plist` already expand
`MARKETING_VERSION` and `CURRENT_PROJECT_VERSION`. Pass the same release version
and build number to both targets; never hand-edit the generated bundle plists.
Record the source revision, Xcode/SDK version and build inputs with each archive.
Increment the build number for a new submitted build.

The following is a template for an authorized **local** archive after the
signing prerequisites below are satisfied, not a command run by this work:

```sh
xcodebuild -project apps/ios/Holon.xcodeproj -scheme Holon \
  -configuration Release -destination 'generic/platform=iOS' \
  -archivePath "$ARCHIVE_PATH" \
  MARKETING_VERSION="$RELEASE_VERSION" CURRENT_PROJECT_VERSION="$BUILD_NUMBER" \
  DEVELOPMENT_TEAM="$TEAM_ID" HOLON_APP_GROUP="$APP_GROUP_ID" archive
```

Set these environment inputs explicitly. Keep archive outputs outside the
source tree. Do not add automatic provisioning-update flags without separate
authorization. An unsigned simulator build is not a signing check.

## Identity and signing prerequisites

- Choose the authorized Apple team and distinct registered app and extension
  Bundle IDs. Current `run.holon.ios` and `run.holon.ios.share` values are
  development placeholders, not proof of registration or ownership.
- Configure each target's `PRODUCT_BUNDLE_IDENTIFIER` separately; a global
  command-line override would incorrectly assign one ID to both targets.
- Register/authorize the intended App Group and assign it to **both** identities
  and provisioning profiles. Supply the same `HOLON_APP_GROUP`; inspect the
  resulting app and extension entitlements, not just the source placeholders.
- Preserve and validate the native login callback `run.holon.ios` and daemon
  redirect allowlist together. Changing Bundle IDs alone does not change the
  callback contract.
- Confirm authorized certificates and compatible profiles exist locally.
  Check extension embedding, matching versions, signed entitlements and group
  access in the resulting archive before any export.
- Distribution export method, profiles, App Store Connect record, agreements,
  privacy policy URL and store metadata require owner review. No export/upload
  or Apple account changes are performed by this preparation.

## Privacy evidence and release blockers

| Surface | Current source evidence | Required release review |
| --- | --- | --- |
| Preferences | `Sources/ConnectionStore.swift` uses `UserDefaults.standard` for connection profiles and selection | Resolve the approved UserDefaults required-reason entry against Apple's current original reference; do not guess a reason code |
| Authentication | `Sources/CredentialVault.swift`, `Sources/NativeLoginProof.swift`: Keychain sessions and recovery proof; native browser login | Validate login, cancellation, logout and server revocation on device; explain administrator access |
| Network | `Info.plist`: Local Network explanation and broad ATS exception; profile HTTP consent | Test LAN denial/recovery and HTTPS; justify broad ATS exception for arbitrary user-chosen daemons |
| User content | Reading cache, drafts, queue, copied attachments and shared inbox | Document retention/deletion limits; assess daemon/operator processing and backups |
| Diagnostics | `Sources/DiagnosticExport.swift`: allowlisted status/count report, explicit export/send | Review report and destinations; do not treat manual export as zero data transmission |
| Permissions | No camera, microphone, photo-library or location purpose keys in the current plists | Re-audit if a feature or SDK begins requesting these permissions |

**Privacy manifest is an unresolved release gate.** This slice deliberately
adds no speculative `PrivacyInfo.xcprivacy`. Confirm the precise approved
reason for the observed UserDefaults use, audit the final dependency/binary
API inventory, then add truthful per-bundle manifests and target resources as
needed. Inspect the archive's privacy report and actual bundled manifests.
Do not declare unobserved file-timestamp, disk-space or uptime categories just
because Foundation is linked. Absence of a manifest here is not an exemption.

Apple's original references to consult:

- `https://developer.apple.com/documentation/bundleresources/describing-use-of-required-reason-api`
- `https://developer.apple.com/documentation/bundleresources/privacy-manifest-files`
- `https://developer.apple.com/app-store/app-privacy-details/`

App Store privacy answers require a separate owner assessment of the shipped
app, SDKs, configured daemon and authentication services, purposes, linking,
retention and any tracking. This is not a blanket “data not collected”
declaration. Supply a public privacy policy that matches the actual deployment.

## Review access and acceptance

- Prepare a dedicated nonproduction review account and disposable agents/data.
  Supply credentials through the authorized review channel, not this repository.
- Provide a reachable authenticated HTTPS daemon throughout review, its API
  base/proxy prefix and step-by-step login instructions. A private LAN-only host
  or developer loopback is not adequate remote reviewer access.
- If OIDC is offered, ensure the review account can complete browser login and
  explain any MFA/access restrictions. Provide a supported alternative only
  when the configured daemon actually offers it.
- Give reviewers steps to read, queue a prompt, inspect work/files, stop an
  observed run, stage a share then explicitly confirm it in the host, export
  diagnostics and log out. Explain that unknown queue outcomes require care.
- On signed physical devices validate app/extension group access, share-sheet
  invocation and host import; Local Network grant/denial; native browser
  callback; logout/revocation; accessibility and cleanup/storage errors.
- Archive inspection, authorized distribution signing/export, TestFlight and
  App Store review remain unverified. Existing local/simulator tests do not
  satisfy these gates. Keep release blocked until privacy and signing evidence
  is recorded by the responsible owners.
