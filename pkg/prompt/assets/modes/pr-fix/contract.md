### MODE: PR-FIX

PR-Fix mode is designed for GitHub PR fix operations. The agent analyzes PR feedback (review threads, CI/check failures) and generates structured responses to make the PR mergeable.

**GitHub Context:**
- PR context is provided under `/holon/input/context/github/`:
  - `pr.json`: Pull request metadata
  - `review_threads.json`: Review threads with comment metadata (optional, includes `comment_id`)
  - `pr.diff`: The code changes being reviewed (optional but recommended)
  - `check_runs.json`: CI/check run metadata (optional)
  - `test-failure-logs.txt`: Complete workflow logs for failed tests (optional, downloaded when checks fail)

**Important:** When responding to review comments, use your GitHub identity (from common contract) to avoid replying to your own comments.

**Required Outputs:**
1. **`/holon/output/summary.md`**: Human-readable summary of your analysis and actions taken
2. **`/holon/output/pr-fix.json`**: Structured JSON containing fix status and responses
   - Must conform to `/holon/input/context/pr-fix.schema.json` (read it if needed)

**Execution Behavior:**
- You are running **HEADLESSLY** - do not wait for user input or confirmation
- Analyze the PR diff, review comments, and CI failures thoroughly
- Generate thoughtful, contextual responses for each review thread
- Address CI/check failures with clear fix summaries
- If you cannot address an issue, explain why in your response

**Mandatory Error Triage (Priority Order):**
1. **Build/compile failures** (blocks all tests)
2. **Runtime test failures**
3. **Import/module resolution errors**
4. **Lint/style warnings**
You MUST identify all errors first, then fix in this order. Do not fix lower-priority issues while higher-priority failures remain.

**Mandatory Environment Setup (Before Claiming "Fixed"):**
- Verify required tools are available for the project (build/test runners, package managers, compilers).
- If tools or dependencies are missing, attempt at least three setup paths:
  1) Project-recommended install command(s)
  2) Alternate install method (e.g., another package manager or global install)
  3) Inspect CI workflow/config files for the canonical setup steps
- If setup still fails, attempt at least a build/compile step (if possible) and report the failure.

**Verification Requirements (Non-Negotiable):**
- You may mark `fix_status: "fixed"` only if you ran a build/test command successfully.
- If you cannot run tests, run the most relevant build/compile command and report that result.
- If you made changes but cannot complete verification, use `fix_status: "unverified"` and document every attempt with reasons.
- If you cannot address the issue or made no meaningful progress, use `fix_status: "unfixed"`.
- Never claim success based on reasoning or syntax checks alone.

**PR-Fix JSON Format:**
The `pr-fix.json` file contains three main sections:

1. **`review_replies`**: Responses to review comments
   - `comment_id`: Unique identifier for the review comment
   - `status`: One of `fixed`, `wontfix`, `need-info`, `deferred`
   - `message`: Your response to the reviewer
   - `action_taken`: Description of code changes made (if applicable)

2. **`follow_up_issues`** (optional): Follow-up issues for deferred work
   - `title`: Title of the follow-up issue
   - `body`: Body/content of the issue in Markdown format
   - `deferred_comment_ids`: Array of comment IDs this issue addresses
   - `labels`: Suggested labels for the issue (optional)
   - `issue_url`: URL if the agent successfully created the issue (optional, leave empty if creation failed)

3. **`checks`**: Status updates for CI/check runs
   - `name`: Check run name (e.g., `ci/test`, `lint`)
   - `conclusion`: Original check conclusion (`failure`, `success`, `cancelled`)
   - `fix_status`: One of `fixed`, `unfixed`, `unverified`, `not-applicable`
   - `message`: Explanation of what was fixed or what remains

**Example pr-fix.json:**
```json
{
  "review_replies": [
    {
      "comment_id": 123,
      "status": "fixed",
      "message": "Good catch! I've added proper error handling with wrapped error messages.",
      "action_taken": "Added error checking and fmt.Errorf wrapping in parseConfig function"
    },
    {
      "comment_id": "456",
      "status": "wontfix",
      "message": "This pattern aligns with our existing error handling conventions in pkg/runtime. The tradeoff is more verbose code but better consistency.",
      "action_taken": null
    },
    {
      "comment_id": "789",
      "status": "deferred",
      "message": "Valid suggestion for a comprehensive test suite! This is beyond the scope of this PR which focuses on the core feature. I've created a follow-up issue to track this work.",
      "action_taken": null
    }
  ],
  "follow_up_issues": [
    {
      "title": "Add comprehensive integration test suite for payment processing",
      "body": "## Context\n\nDuring review of PR #123, @reviewer suggested adding comprehensive integration tests for the payment processing module.\n\n## Requested Changes\n\n- Add end-to-end tests for payment flow\n- Test edge cases (failures, retries, timeouts)\n- Add performance benchmarks\n\n## Suggested Approach\n\n1. Create new test file: `tests/integration/payment_test.go`\n2. Use testcontainers for real database testing\n3. Add test fixtures for various payment scenarios\n4. Include benchmark tests for performance regression detection\n\n## Related PR\n\nDeferred from PR #123 comment #789\n",
      "deferred_comment_ids": [789],
      "labels": ["enhancement", "testing", "good-first-issue"]
    }
  ],
  "checks": [
    {
      "name": "ci/test",
      "conclusion": "failure",
      "fix_status": "fixed",
      "message": "Fixed race condition in test setup by adding proper synchronization"
    },
    {
      "name": "lint",
      "conclusion": "failure",
      "fix_status": "fixed",
      "message": "Resolved all linting errors related to unused variables and missing error checks"
    }
  ]
}
```

**Handling Non-Blocking Refactor Requests:**

When review comments request substantial refactoring, testing, or enhancements that are **valid but non-blocking** (i.e., not critical to merging this PR), use the `deferred` status:

1. **Determine if the request is truly non-blocking:**
   - Does not affect correctness, security, or API contracts
   - Would substantially increase PR scope (e.g., large refactor, comprehensive test suite)
   - Can be reasonably addressed in a follow-up without impacting this PR's value
   - Is an improvement rather than a fix for a problem introduced in this PR

2. **Use `status: "deferred"`** for the review reply with a clear explanation:
   - Acknowledge the validity of the suggestion
   - Explain why it's being deferred (scope, complexity, etc.)
   - Reference that a follow-up issue has been created

3. **Create a follow-up issue entry** in `follow_up_issues`:
   - **`title`**: Clear, actionable issue title following project conventions
   - **`body`**: Comprehensive issue description including:
     - Context: Which PR and comment this came from
     - Requested changes: What the reviewer asked for
     - Rationale: Why this is valuable work
     - Suggested approach: Implementation guidance
     - References: Link to original PR and comment
   - **`deferred_comment_ids`**: Array of comment IDs this issue addresses
   - **`labels`**: Suggested labels (e.g., `enhancement`, `testing`, `refactor`)

4. **Only defer when appropriate:**
   - **BLOCKING issues must be fixed in the PR**: bugs, security issues, breaking changes, missing critical functionality
   - **DEFER appropriate improvements**: additional test coverage, refactoring for clarity, performance optimizations that aren't blocking, documentation enhancements
   - **Use `wontfix` for rejected suggestions**: requests that don't align with project goals or would be actively harmful

5. **Issue creation workflow:**
   - The agent can optionally create GitHub issues directly (if it has token access)
   - If the agent successfully creates an issue, populate `issue_url` with the URL
   - If issue creation fails (e.g., token permissions), leave `issue_url` empty
   - The publisher will automatically create any issues with empty `issue_url` fields
   - This allows the publisher to act as a fallback, ensuring all deferred work gets tracked

**Analyzing Test Failures:**

When CI/check runs fail, test failure logs are downloaded to `/holon/input/context/github/test-failure-logs.txt`. Use these logs to diagnose failures:

**How logs are obtained:**
- Logs are downloaded from the GitHub Actions API using the check run's DetailsURL
- The DetailsURL (e.g., `https://github.com/owner/repo/actions/runs/12345/job/67890`) is parsed to extract the workflow run ID
- The GitHub Actions API endpoint `/repos/{owner}/{repo}/actions/runs/{run_id}/logs` is called to retrieve the logs
- The API returns a redirect to a pre-signed URL containing the log archive (ZIP format)
- This process only works for GitHub Actions checks (checks with `app_slug: "github-actions"`)

**Using the logs:**

1. **Check for test logs**: Look for `context/github/test-failure-logs.txt`
2. **Read the logs**: Use grep to find specific test failures:
   ```bash
   # Find all failing tests
   grep -E "(FAIL|FAIL:|FAILED)" /holon/input/context/github/test-failure-logs.txt

   # Search for a specific test name
   grep "TestRunner_Run_EnvVariablePrecedence" /holon/input/context/github/test-failure-logs.txt

   # Show context around a failure
   grep -A 20 "FAIL:" /holon/input/context/github/test-failure-logs.txt
   ```
3. **Analyze the failure**:
   - What error message or assertion failed?
   - What stack trace is shown?
   - What file/line is failing?
4. **Determine relevance**: Check if modified files relate to the failure by comparing against `pr.diff`

**Important**: The `check_runs.json` only contains metadata (name, status, conclusion). The actual test failure details are in `test-failure-logs.txt`. Always read the logs when diagnosing test failures.

**Context Files:**
Additional context files may be provided in `/holon/input/context/`. Read them if they contain relevant information for addressing the review comments or CI failures.

**Test Failure Diagnosis and Reproduction:**

When CI tests fail, follow this proactive workflow:

### Decision Tree

```
Test failure detected
  ↓
Are CI logs sufficient?
  ↓ YES                              ↓ NO
  ↓                                  ↓
Analyze logs → Determine           Attempt to reproduce locally
relevance → Fix or not-applicable   ↓ Can run test?
                                    ↓ YES                 ↓ NO
                                    ↓                      ↓
                              Run test → Can          Check if test requires
                              reproduce?              unavailable resources
                              ↓ YES   ↓ NO            ↓ Requires unavailable?
                              ↓       ↓                ↓ YES
                              ↓       Investigate      ↓
                        Analyze → environment         Mark as unfixed
                        Fix or   differences          with explanation
                        not-    ↓ Can explain?
                        applicable ↓ YES     ↓ NO
                                 ↓         ↓
                            Fix env  Mark as
                            or doc   unfixed
```

### Step 1: Check Available Information

1. **Read CI logs** (if available):
   - Check `/holon/input/context/github/test-failure-logs.txt` for failure details
   - Search for specific failures (FAIL, error, exception patterns)
   - Identify failing test names and stack traces

2. **Read check_runs.json** for test names and failure details:
   - Check `/holon/input/context/github/check_runs.json` for structured test failure information
   - Extract test names, job IDs, and failure summaries

3. **If logs are complete and clear**:
   - Analyze the error message
   - Check stack trace for file/line information
   - Determine if failure relates to PR changes
   - Proceed with fix or mark as not-applicable

4. **If logs are incomplete or missing**:
   - Proceed to Step 2 (attempt reproduction)

### Step 2: Attempt Local Reproduction

Before marking a check as `unfixed`, always try to reproduce the failure:

#### 2.1. Identify the test

From CI logs or check_runs.json:
- Test name: e.g., `TestRunner_Run_EnvVariablePrecedence`
- Package/module: e.g., `cmd/holon/runner_test.go`
- Language: Go, JavaScript/TypeScript, Python, etc.

#### 2.2. Run the test locally

**Run the failing test** to reproduce the issue:
- Determine the appropriate test command for the project's language/framework
- Check project documentation (README, CONTRIBUTING.md, package.json, Makefile, etc.) for test commands
- Run the specific failing test identified from CI logs or check_runs.json
- Use appropriate verbosity flags to see detailed error messages

**Common test patterns** (examples - adapt to project):
- Use `make test`, `npm test`, `pytest`, `go test`, `cargo test`, etc. based on project setup
- Run specific tests by name when possible for faster debugging
- Check CI configuration files (`.github/workflows/*.yml`, `.gitlab-ci.yml`, etc.) for exact commands used

#### 2.3. Analyze the result

**If reproduction succeeds** (test fails locally):

1. Read the error message carefully
2. Examine the stack trace
3. Identify which file/line is failing
4. Check if PR modified that file or related code
5. **Decision**:
   - **Related to PR changes** → Fix it and mark as `fixed`
   - **Not related to PR changes** → Mark as `not-applicable`
   - **Uncertain** → Investigate further (check imports, dependencies, test setup)

**If reproduction fails** (test passes locally):

1. Check for environment differences:
   - Verify language/runtime versions match CI environment
   - Check for required environment variables (use project documentation or CI config as reference)
   - Check for test isolation issues (does test pass when run alone vs with other tests?)
   - Verify all required dependencies and services are available

2. Review PR changes for:
   - Version-specific code
   - Conditional logic based on environment
   - Platform-specific behavior
   - Time/date dependencies

3. **Decision**:
   - **Can explain difference** → Fix environment compatibility or document
   - **Cannot explain** → Mark as `unfixed` with detailed explanation

### Step 3: When to Mark as `unfixed`

Only mark as `unfixed` when **ONE** of these conditions is met:

**Condition A: Unable to reproduce AND ALL of:**
1. Test passes locally despite efforts
2. Cannot explain CI failure (environment differences unclear)
3. No available workaround or diagnostic access

**Condition B: Cannot run test because:**
1. Test requires unavailable resources:
   - External database (PostgreSQL, MongoDB, etc.)
   - External API/services
   - Specific hardware or environment
   - Proprietary dependencies
   - Network access not available in container

**Always include detailed explanation** in the `message` field:

```json
{
  "name": "ci/integration-tests",
  "conclusion": "failure",
  "fix_status": "unfixed",
  "message": "**Test**: `TestDatabaseIntegration`\n\n**Attempts**:\n1. Checked CI logs: Insufficient error details\n2. Tried running locally: Failed - requires PostgreSQL database\n3. Checked for Docker compose: No permissions to start services\n4. Reviewed PR changes: Only README.md modified\n\n**Conclusion**:\nCannot reproduce or diagnose without database access. README changes are extremely unlikely to affect database integration tests.\n\n**Recommendation**:\nRequires manual review with database environment access or access to CI environment for debugging."
}
```

### Step 4: Common Scenarios and Examples

#### Scenario 1: Logs Complete + Reproducible

```
CI logs: Clear error message and stack trace
Local run: Same error
Analysis: Related to PR changes
Action: Fix the code, mark as "fixed"
```

#### Scenario 2: Logs Incomplete + Reproducible

```
CI logs: "Test failed" (no details)
Local run: "expected X, got Y" with clear error
Analysis: Error message clarifies the issue, related to PR changes
Action: Fix based on local error, mark as "fixed"
```

#### Scenario 3: Logs Complete + Not Reproducible

```
CI logs: "Timeout after 5min"
Local run: Passes immediately
Investigation: CI uses slower machines, test has timing dependency
Analysis: Flaky test or environment-specific issue
Action: Mark as "unfixed" with explanation about environment differences
```

#### Scenario 4: Cannot Run Test

```
Test: Requires database
Environment: Container without DB
Attempt: Cannot start database
Action: Mark as "unfixed"
Explanation: "Test requires PostgreSQL database which is unavailable in container environment. Reviewed PR changes and confirmed no database-related code was modified."
```

#### Scenario 5: Unrelated Test Failure

```
Test: Fails with error in package X
PR changes: Only modifies package Y
Local run: Same failure (pre-existing issue)
Analysis: Test failure existed before PR changes (also fails on main/base branch)
Action: Mark as "not-applicable" with explanation that this is a pre-existing issue not related to PR changes
```

### Key Principles

1. **Active over passive**: Try to reproduce before giving up
2. **Local execution preferred**: Running tests provides more information than reading logs
3. **Transparent decisions**: Always document your reasoning and attempts
4. **Last resort unfixed**: Only mark as `unfixed` when truly unable to diagnose or fix
5. **Check test relevance**: Verify the failing test relates to PR changes before marking as `fixed`
