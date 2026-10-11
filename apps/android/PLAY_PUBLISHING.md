# Privacy and compliance preparation

See [Play compliance evidence and release gates](PLAY_COMPLIANCE.md) before
completing Console declarations. The policy pages and in-app link are prepared;
their public URLs must be deployed and verified separately before submission.

# Google Play internal testing

The **Android Play internal testing** workflow is a separate, manual publishing
entry point. It downloads an already published stable GitHub Release's signed
AAB, verifies its checksums, metadata and pinned signing certificate, then uploads
it to `run.holon.android` on the `internal` track. It does not rebuild the app,
publish on tag pushes, or allow selecting production/open/closed tracks.
The existing [signing and release workflow](./RELEASING.md) stays build-only.

## One-time setup

1. Create the Play Console app and enroll it in Play App Signing. Its package
   name must remain `run.holon.android`. Preserve the existing release keystore
   as the upload key; do not generate a replacement key for CI.
2. For a new app, create the first **internal testing** release in Play Console
   and manually upload the official `holon-android-vX.Y.Z.aab` from a GitHub
   Release. A Console listing alone is not enough for API uploads. Start with a
   draft if the app setup is incomplete; this is not a production rollout.
3. In a Google Cloud project, enable the **Google Play Android Developer API**
   and create a dedicated service account. Google no longer requires linking
   the developer account to a Cloud project. The publisher does not need a
   Cloud project Owner, Editor, or other project role just to publish to Play.
4. Invite the service account email under Play Console **Users and permissions**.
   Scope access to this app: viewing app information and releasing apps to
   testing tracks. Do not grant production release permissions or account admin
   access. Check the current Console labels and ensure the invitation is active.
   If the account also maintains listing content, grant this app's store-presence
   permission separately. Cloud IAM access does not grant Play app access.
5. Store the service account's JSON private key as the GitHub Actions repository
   secret `GOOGLE_PLAY_SERVICE_ACCOUNT_JSON`. Do not paste it into chat, commit
   it, or include it in workflow logs. Revoke/rotate it if it is disclosed.
   The upload job does not need the signing-keystore secrets.
6. Configure internal testers and distribute the opt-in link in Play Console.
   Store listing, privacy/data-safety declarations and review access are separate
   obligations; an API upload does not complete them.

Use Google's current [API setup guide](https://developers.google.com/android-publisher/getting_started)
for the Cloud and Play permission steps.

### Verify authentication before publishing

Creating the service account and storing the Secret only completes the Cloud
and GitHub parts of setup. Play Console's app-scoped grant is still required.
An ordinary `gcloud auth login` token may lack the `androidpublisher` OAuth
scope; use service-account credentials with that scope for Publisher API calls.
Do not change the machine's default Cloud project or active user to run CI.

Keep JSON credentials outside the repository, use restrictive file permissions,
and pass them to `gh secret set GOOGLE_PLAY_SERVICE_ACCOUNT_JSON --repo owner/repo`
through stdin. Remove temporary local copies once stored. Verify only Secret
names, key metadata and sanitized API results; never print the key or token.
A scoped token followed by a Play `403` means access is still blocked; do not
report a successful upload based on token creation or Secret configuration.

## Store listing and app content

The proposed English title and descriptions are in
[play/en-US/listing.json](./play/en-US/listing.json). They describe the native
client and explicitly require access to a Holon runtime. The upload workflow
does not synchronize this file or complete Play Console's app-content forms.
Preserve existing listing fields and languages when applying this copy.

Before declaring the app store-ready, the account holder must supply or confirm:

- The app icon, feature graphic and actual app screenshots. Do not upload
  automated visual-test fixtures as store screenshots.
  Prepared English assets and capture provenance are in
  [play/README.md](play/README.md); these assets have not been uploaded.
- The public privacy-policy URL, data-safety answers and retention/deletion
  behavior, based on the app and the runtimes users connect to.
- Content rating, target audience, ads and any other required declarations.
- Public support/contact details and working review-access instructions or
  credentials when required. Do not publish personal contact details or secrets
  without the account holder's confirmation.

These are Console/review requirements, not consequences of creating a service
account or completing an internal release. Do not infer declarations from
successful API authentication, and do not cancel an existing review or send
unrelated pending changes for review while maintaining listing copy.

If a listing-only API edit cannot be committed with
`changesNotSentForReview=true` and
`changesInReviewBehavior=ERROR_IF_IN_REVIEW`, delete the temporary edit and
retain the proposed copy for the account holder to apply in Console. Do not
retry without those safeguards just to bypass automatic review submission.

## Publish an internal release

Once the workflow is on `main` and one-time setup is complete:

First check Play Console for changes in review or unrelated pending changes.
Do not run this workflow until those changes have been resolved by the account
holder. The pinned upload action commits a Play edit and does not expose the
Publisher API's `changesInReviewBehavior=ERROR_IF_IN_REVIEW` safeguard; an API
commit can affect the app's existing review state even though the workflow only
updates the internal track.

1. Choose **Actions → Android Play internal testing → Run workflow**, using the
   `main` branch.
2. Set `version` to a published stable tag, for example `vX.Y.Z`. Draft and
   prerelease GitHub Releases are rejected, as are bundles with incorrect
   metadata, checksums or signing identity.
3. Choose `draft` (the default) to save the internal release without making it
   available to testers, or `completed` to release to internal testers only.
   Apps still in draft setup may only accept draft releases. No production
   submission is performed by either choice.
4. Check the Actions summary and Play Console internal track for the matching
   `versionCode`, status and bundle. Google may still apply processing or review
   requirements before testers can download it.

Do not upload a versionCode already uploaded manually or by a previous API run.
For the first manually uploaded bundle, finish its draft in Console rather than
running this upload workflow on the same tag again. Subsequent uploads use the
next Holon release; never repackage changed code under an existing version.

Concurrent runs are serialized and are not cancelled mid-upload. If upload or
commit fails, inspect Play Console before retrying: the bundle/versionCode may
already have been accepted. Do not bypass a rejection by changing signing keys,
package names, or disabling verification.

## Google Play Alpha candidates

The **Android Play Alpha** workflow builds an unpublished Android-only candidate
from `main`; it does not require or create a GitHub Release, update the runtime
version in Git, or write to Production. Use this when Android fixes have merged
since the latest stable release.

1. Check Play Console for unrelated pending changes and changes in review.
   Resolve them first: an API edit can submit other already-pending app changes.
2. After the workflow is merged, choose **Actions → Android Play Alpha → Run
   workflow**, selecting `main`. Set an explicit unused stable candidate version
   newer than `Cargo.toml` and all uploaded Play version codes, such as `v0.50.1`.
   The version override exists only in the runner's checkout.
3. Provide accurate English release notes (at most 500 characters), and confirm
   that no unrelated changes are pending. Choose `draft` (default) to save a
   candidate while retaining the serving Alpha release, or `completed` to submit
   the Alpha release for Google's processing/review.
4. Review the Actions artifact (signed APK/AAB, source commit, certificate and
   checksums), summary and Play Console. API success does **not** mean that the
   candidate has passed review or is downloadable by testers.

The workflow reuses the four `ANDROID_SIGNING_*` Secrets and
`GOOGLE_PLAY_SERVICE_ACCOUNT_JSON`. The service account must have app-scoped
permission to view the app and release to testing tracks; no production
permission is required. Pull requests run credential-free contract tests;
upload/signing can only run on a manual `main` dispatch.
The local contract test uses Python 3.11 or newer:
`python3.11 -B scripts/test-android-play-alpha.py`; Actions provisions Python 3.12.

The publisher requires an existing `alpha` track bound to
`holon-pioneers@googlegroups.com`. It rejects existing Alpha drafts/rollouts,
reused or lower version codes, and bundle hash/version mismatches. It updates
only Alpha, verifies other tracks and tester settings are unchanged, validates
the edit, and commits with `changesInReviewBehavior=ERROR_IF_IN_REVIEW`.
Unlike the internal upload action, it will not cancel an existing review, retry
with weaker safeguards, or silently defer a failed submission to Console.
Committed settings are read back in a fresh edit; that read-only edit is deleted.
Alpha and Internal uploads share a concurrency group and are not cancelled
mid-upload. Do not make concurrent manual Console edits.

If an upload, commit or readback fails, inspect Console before retrying: the
version code may already have been accepted. In particular, a saved draft must
be completed in Console rather than re-uploaded using the same version code.
Do not disable verification, replace the signing identity or grant production
permissions to work around a failure.
