export function summarizeRun(result) {
  if (result.status === "processed") {
    return `processed ${result.kind}`;
  }

  return "idle";
}
