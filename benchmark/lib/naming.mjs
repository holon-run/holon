export const CANONICAL_RUNNER_IDS = [
  "holon-openai",
  "codex-openai",
  "holon-anthropic",
  "claude-cli"
];

export function branchNameForTask(taskId, runnerId) {
  return `bench/${taskId}/${runnerId}`;
}

export function worktreeNameForTask(issueNumber, runnerId) {
  return `bench-${String(issueNumber).padStart(4, "0")}-${runnerId}`;
}

export function prTitleForTask(issueNumber, issueTitle, runnerId) {
  return `[bench][${runnerId}][#${issueNumber}] ${issueTitle}`;
}

export function benchmarkLabelsForTask(issueNumber, runnerId) {
  return ["bench", `bench:task-${issueNumber}`, `runner:${runnerId}`];
}

export function artifactDirForTask(resultsRoot, suiteLabel, taskId, runnerId, repetition = 1) {
  const runId = `run-${String(repetition).padStart(2, "0")}`;
  return { runId, path: `${resultsRoot}/${suiteLabel}/${taskId}/${runnerId}/${runId}` };
}
