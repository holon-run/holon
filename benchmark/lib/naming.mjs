export function branchNameForTask(taskId, runnerId, repetition = 1, suiteLabel = "adhoc") {
  return `bench/${stableSlug(suiteLabel)}/${taskId}/${runnerId}/run-${String(repetition).padStart(2, "0")}`;
}

export function worktreeNameForTask(issueNumber, runnerId, repetition = 1) {
  return `bench-${String(issueNumber).padStart(4, "0")}-${runnerId}-run-${String(repetition).padStart(2, "0")}`;
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

function stableSlug(value) {
  return (
    String(value)
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "") || "run"
  );
}
