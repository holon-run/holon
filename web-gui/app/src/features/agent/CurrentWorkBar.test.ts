import { describe, expect, it } from "vitest";
import { deriveCurrentWorkStatus } from "./CurrentWorkBar";
import type { AgentSummary } from "../../runtime/types";

const agent = {
  id: "a", lifecycle: "awake-idle", currentRunId: null, posture: "idle",
  currentWork: { id: "w", objective: "Run checks", state: "open" },
  tasks: [{ id: "t", workItemId: "w", status: "running", kind: "command_task", summary: "Run tests" }],
  waits: [],
} as unknown as AgentSummary;
const wait = { id: "wait", work_item_id: "w", kind: "task", status: "active", task_ids: ["t"], created_at: "2026-09-16T00:00:00Z" };

describe("current work status", () => {
  it("does not infer waiting just because a background task exists", () => {
    expect(deriveCurrentWorkStatus(agent)).toMatchObject({ state: "background" });
  });
  it("prefers the current work's explicit wait over aggregate agent posture", () => {
    expect(deriveCurrentWorkStatus({ ...agent, waits: [wait], waitingReason: "awaiting_operator_input", posture: "waiting-for-operator" }).state).toBe("waitingTask");
  });
  it("uses explicit task bindings", () => {
    expect(deriveCurrentWorkStatus({ ...agent, waits: [wait] })).toMatchObject({ state: "waitingTask", waitingTask: { id: "t" } });
  });
  it("does not attribute another work item's wait to current work", () => {
    expect(deriveCurrentWorkStatus({ ...agent, waitingReason: "awaiting_task_result", waits: [{ ...wait, work_item_id: "other" }] })).toMatchObject({ state: "background" });
  });
  it("shows active execution while background waits remain", () => {
    expect(deriveCurrentWorkStatus({ ...agent, currentRunId: "run", waits: [wait] })).toMatchObject({ state: "running" });
  });
  it("keeps triggered results distinct from active waits", () => {
    expect(deriveCurrentWorkStatus({ ...agent, tasks: [], waits: [{ ...wait, status: "triggered" }] })).toMatchObject({ state: "resultReady" });
  });
  it("supports older servers without explicit wait records", () => {
    expect(deriveCurrentWorkStatus({ ...agent, waitingReason: "awaiting_task_result" })).toMatchObject({ state: "waitingTask", waitingTask: undefined });
  });
  it("shows stopped and operator-input states explicitly", () => {
    expect(deriveCurrentWorkStatus({ ...agent, lifecycle: "stopped", waits: [wait] }).state).toBe("stopped");
    expect(deriveCurrentWorkStatus({ ...agent, waitingReason: "awaiting_operator_input" }).state).toBe("needsInput");
  });
  it("finds the waiting owner when there is no current focus", () => {
    expect(deriveCurrentWorkStatus({ ...agent, currentWork: undefined, workItems: [agent.currentWork!], waits: [wait] }).work?.id).toBe("w");
  });
});
