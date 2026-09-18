import type { TestInfo } from "@playwright/test";

// The fixture server keys all scripted state (configured fixtures, request
// counters, scripted sequences) by session id for the whole run. Every attempt,
// including retries, must use its own session, or a failed attempt leaks that
// state into the retry and turns a transient timeout into a permanent failure.
export function sessionFor(testInfo: TestInfo, prefix = "e2e"): string {
  const testCase = testInfo.testId.replace(/[^a-zA-Z0-9_-]/g, "-");
  return `${prefix}-${testInfo.workerIndex}-${testInfo.repeatEachIndex}-${testInfo.retry}-${testCase}`;
}
