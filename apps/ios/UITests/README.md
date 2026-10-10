# Native UI smoke contract

These tests drive the shipped app through XCUITest; they do not seed credentials,
replace transports, or install production test hooks. Use a fresh, isolated
simulator owned by the invoking harness. A simulator containing a prior logged-in
installation is not the disconnected fixture.

Set `IOS_SIMULATOR_ID`, `IOS_DERIVED_DATA_PATH`, and an unused
`IOS_RESULT_BUNDLE_PATH`, then run through the repository UI harness:

```sh
make ios-ui-test
```

### Content-report acceptance

After rebasing onto the real content-report API, run on the isolated simulator:

```sh
IOS_RICH_ACCEPTANCE=1 IOS_CONTENT_REPORT_ACCEPTANCE=1 \
IOS_UI_CASES=testContentReportConfirmationCancelDoesNotPersist,testContentReportInvalidExplanationCannotShowAccepted,testContentReportAcceptedReceiptCannotSubmitTwice \
make ios-ui-test
```

These English/light/system-`large` cases reuse shipped pairing and reading UI.
They select a real assistant transcript, cancel native confirmation without a
write, reject a 2,001-character explanation without fake acceptance, and confirm
a real accepted receipt with a `report_…` ID. Accepted drafts cannot submit again,
including after foreground restoration. The harness checks the actual SQLite
report count after each case, and checks the accepted row's agent, turn, category,
explanation, content snapshot, persisted `received` status and client request ID.
It never prints credentials. This is not server-error or lost-response coverage:
that requires a separately authorized failure fixture.

The harness must explicitly export the following `TEST_RUNNER_` variables to
`xcodebuild`. Xcode forwards them into the test runner with that prefix removed.
The runner reads `HOLON_UI_*`; ordinary shell `HOLON_UI_*` variables alone are
not the forwarding contract. None are passed to the application environment.

| Exported variable | Required fixture value |
| --- | --- |
| `TEST_RUNNER_HOLON_UI_CONTENT_SIZE` | Verified simulator system text size: `large` or `accessibility-extra-extra-extra-large`, according to the case |
| `TEST_RUNNER_HOLON_UI_ENDPOINT` | Complete isolated daemon API URL ending `/api` |
| `TEST_RUNNER_HOLON_UI_PAIRING_CODE` | Fresh, single-use 64-hex pairing ticket |
| `TEST_RUNNER_HOLON_UI_AGENT_ID` | Visible test agent with reading snapshot |
| `TEST_RUNNER_HOLON_UI_WORK_ID` | Visible WorkItem belonging to that agent |
| `TEST_RUNNER_HOLON_UI_TASK_ID` | Visible task belonging to that agent |
| `TEST_RUNNER_HOLON_UI_READ_MARKER` | Literal text in a visible conversation turn |
| `TEST_RUNNER_HOLON_UI_PLAN_MARKER` | Literal text beyond the truncated plan preview |
| `TEST_RUNNER_HOLON_UI_TASK_MARKER` | Literal text in real task output |
| `TEST_RUNNER_HOLON_UI_FILE_REFERENCE` | Resolvable fixture file reference |
| `TEST_RUNNER_HOLON_UI_FILE_MARKER` | Literal text in that file's native preview |

Missing authenticated fixture inputs fail the test; they never skip it. The
fixture must accept the explicit message and return a received outbox receipt.
The native login payload is derived from the supplied endpoint by replacing the
final `/api` with `/login` and adding `#pair=<ticket>`. Tests explicitly operate
both profile HTTP permission and pairing HTTP permission.

Two disconnected cases cover English/light and Simplified Chinese/dark with
accessibility XXXL text. The harness sets and reads back the real simulator
`simctl ui content_size`. It first uses `simctl bootstatus <UUID> -b` to boot its
fresh, owned simulator if needed and wait until ready; failure stops the suite
before text-size changes. It then explicitly initializes and verifies a `large`
suite baseline on that simulator, whose unset initial category may
be reported as `unknown`. This is fixture configuration, not an interpretation
of `unknown` as `large`. Before each case it reads the baseline, sets and verifies
the required size, then restores and verifies that baseline even if configuration
or XCTest fails. Unreadable categories and failed readbacks still fail the suite.
Do not replace this with an application-only launch override.
Three diagnostic regressions cover control text shrinking from
maximum to ordinary size, prepared text growing at runtime, and maximum-size
viewport coverage. Each case runs separately with its required initial size.
XCUITest accessibility audits run without ignored
findings or generated screenshot baselines. Screenshots are retained xcresult
attachments. The authenticated workflow covers reading, sending/received,
Work detail/full plan, task output, file preview and diagnostic-send confirmation.

Set `IOS_SHARE_ACCEPTANCE=1` to add `testDirectAgentShareWorkflow` after onboarding.
The harness builds a separate native sender app and drives the real system
sheet/embedded extension for text, web URL, image and file, requiring the Agent's
receipt, authoritative conversation inputs and materialized image/file bytes.
CI enables this case. For the extension's lost-response gate, also set
`IOS_LOST_RESPONSE_ACCEPTANCE=1 IOS_UI_CASES=testDirectAgentShareWorkflow`: the
proxy discards the real daemon's initial receipt, then requires the explicit retry
to return a duplicate receipt with the same UUID and message ID.
Its fixture-only sender has no
credentials or production hooks. Signed-device App Group interoperability,
organization browser login and physical-device local-network prompts are not
covered by simulator success.
Test activity logs may include the manually typed, ephemeral pairing payload:
keep xcresult local/restricted; do not publish it as a sanitized diagnostic.
Static parsing/project validation is not evidence these UI tests passed.

## Opt-in review demo

`DemoReviewUITests/testDemoReviewWorkflow` is separate from the isolated CI
fixture. Run it only with explicit permission to interact with
`https://demo.holon.run`, on a new task-owned simulator, selecting
`-only-testing:HolonUITests/DemoReviewUITests/testDemoReviewWorkflow`.
Forward `TEST_RUNNER_HOLON_DEMO_ENDPOINT` (`https://demo.holon.run/api`),
`TEST_RUNNER_HOLON_DEMO_PAIRING_TICKET`, `TEST_RUNNER_HOLON_DEMO_AGENT_ID`, and
`TEST_RUNNER_HOLON_DEMO_REPLY_MARKER` to `xcodebuild`. Missing inputs fail rather
than skip; the ordinary CI harness does not select this live test.

Issue a short-lived, single-use pairing ticket using an authorized temporary
native session. Do not pass the administrator/reviewer token into XCUITest,
application launch arguments, command arguments, screenshots, or public logs.
The ticket can appear in local XCTest activities; retain results privately,
consume it once, revoke the temporary issuance session, and delete the owned
simulator after testing. The test uses the shipped pairing UI, explicit data
sharing consent, one non-sensitive prompt, the server receipt and actual Agent
reply, then removes the saved connection. It does not establish demo isolation,
provider retention rules, physical-camera behavior, or App Store compliance.
