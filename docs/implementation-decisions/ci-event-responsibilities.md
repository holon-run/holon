# CI event responsibilities

Decision:

- Pull requests retain path-scoped build, quality and test gates.
- Main pushes retain path-scoped builds and compile checks, not test execution
  or non-build quality gates.
- The daily `02:40 UTC` schedule and `workflow_dispatch` retain full coverage,
  including the five existing Apple jobs. Their conditions are unchanged.

Mixed entrypoints must select the appropriate command, not skip the whole job:

| Component | Main push | PR / nightly / manual |
| --- | --- | --- |
| Rust | Existing build/check commands; independent crates use `cargo check` | Existing build/check/test commands |
| Web | `make web` (including API, App and Conversation SDK builds) | `make web-ci` |
| Conversation SDK | `npm ci && npm run build` | `make conversation-sdk-ci` |
| Android | Existing APK/AAB tasks, without unit tests | Existing tooling/unit/screenshot/lint and packaging gates |
| Docker | Existing image build | Image build, smoke and E2E validation |

Reason:

The main-push safety net detects compilation/build failures without repeating
PR tests. A successful main-push build is not evidence of logical correctness.
The independent-crate command is selected from the event so one manifest list
continues to cover both compilation and testing, including `holon-web-retrieval`.
`make build` includes `cargo test --no-run`: compiling test targets is allowed;
running them is not.

Boundary:

Path filters, runners, resource budgets, timeouts, retry policy, concurrency,
and existing image publication are unchanged. Main pushes no longer export
images solely for skipped downstream test jobs. Build errors remain fatal;
the timing summary does not replace the underlying job conclusions.

Verification:

`python3 -m unittest tests.test_ci_timing tests.test_ci_test_shards
tests.test_build_script tests.test_ci_event_matrix` checks the static event
matrix and executes the actual mixed shell/Makefile commands with stubbed
tools, including injected build failures. Run `actionlint
.github/workflows/ci.yml` as well. These are not real GitHub event, compiler,
Gradle, Docker or Apple-platform results; report those separately.
