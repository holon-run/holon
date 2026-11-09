# iOS UI Test On-Demand Tier

Decision:

- the isolated simulator UI sweep is not a default PR or push CI gate
- it runs on demand via a manual CI workflow dispatch on the branch to verify,
  or through the scheduled nightly full-sweep safety net
- PRs changing the iOS app keep the core gates: build-tooling
  contracts, the app build, hosted native tests, and
  the real-daemon contract probe
- wire and full SDK checks retain their existing SDK/Rust path filters;
  iOS-only changes do not select them
- runtime-only compatibility changes retain wire, SDK and real-daemon probes
- locally the same 21-case sweep stays available as
  `IOS_SHARE_ACCEPTANCE=1 IOS_TASK_RESULT_ACCEPTANCE=1 make ios-ui-test`,
  with `IOS_UI_CASES` for per-case runs; without the acceptance flags,
  `make ios-ui-test` runs 19 cases

Reason:

- the 21-case sweep measured 29 minutes of pure test execution and
  28–55 minutes per CI step in sampled shared macOS runs; the existing
  workflow documents a retry-inflated tail near 80 minutes
- its failures concentrated in environment-sensitive system flows (system
  share sheet, network interruption, cold launch), and remote evidence
  policy keeps UI xcresults and screenshots on the runner, so per-PR
  failures cost a long round-trip to diagnose

Preserved boundary:

- the nightly full sweep remains scheduled to run every tier daily,
  preserving UI regression coverage outside per-PR gates
- no new PR label events are introduced: ordinary label changes must not
  cancel or restart the whole CI workflow
- removing the sweep from PRs must not be used to declare UI-only
  regressions fixed; evidence still comes from the same sweep run on
  demand or locally
