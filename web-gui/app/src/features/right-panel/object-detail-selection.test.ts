import { describe, expect, it } from "vitest";
import { resolveWorkItemDetail } from "./object-detail-selection";
import type { WorkItemSummary } from "../../runtime/types";

const fallback: WorkItemSummary = { id: "w", objective: "Navigation snapshot", state: "open", current: true };

describe("live WorkItem detail selection", () => {
  it("uses the newer read's plan descriptor when business revisions are equal", () => {
    const plan = { path: "/test/plan.md", hash: "detail" };
    const full = { ...fallback, planArtifact: plan };
    const list = { ...fallback, planArtifact: { ...plan, hash: "list" } };
    expect(resolveWorkItemDetail(fallback, list, { workItem: full, dataRequestId: 1 }, 2).planArtifact?.hash).toBe("list");
    expect(resolveWorkItemDetail(fallback, list, { workItem: full, dataRequestId: 3 }, 2).planArtifact?.hash).toBe("detail");
  });

  it("prefers an explicit newer business revision even if its list was read later", () => {
    expect(resolveWorkItemDetail(
      fallback, { ...fallback, revision: 1 },
      { workItem: { ...fallback, revision: 2, objective: "Newer detail" }, dataRequestId: 1 }, 2,
    ).objective).toBe("Newer detail");
  });

  it("preserves same-version full text after a later State read without reverting live fields", () => {
    const full = {
      ...fallback,
      revision: 3,
      updatedAt: "2026-11-12T19:00:00Z",
      objective: "Full objective. ".repeat(40),
      blockedBy: "Full blocking reason. ".repeat(40),
      readiness: "runnable",
      schedulingState: "runnable",
      planArtifact: { hash: "detail" },
    };
    const list = {
      ...full,
      objective: `${full.objective.slice(0, 160)}…`,
      blockedBy: `${full.blockedBy.slice(0, 160)}…`,
      current: false,
      readiness: "blocked",
      schedulingState: "waiting_for_operator",
      planArtifact: { hash: "state" },
    };
    expect(resolveWorkItemDetail(fallback, list, { workItem: full, dataRequestId: 1 }, 2)).toMatchObject({
      objective: full.objective,
      blockedBy: full.blockedBy,
      current: false,
      readiness: list.readiness,
      schedulingState: list.schedulingState,
      planArtifact: list.planArtifact,
    });
  });

  it.each([
    { revision: 4, updatedAt: "2026-11-12T19:00:00Z" },
    { revision: 3, updatedAt: "2026-11-12T19:01:00Z" },
  ])("does not preserve stale full text over a newer State version %j", (version) => {
    const full = {
      ...fallback,
      revision: 3,
      updatedAt: "2026-11-12T19:00:00Z",
      objective: "Old full objective",
      blockedBy: "Old full blocking reason",
    };
    const list = { ...full, ...version, objective: "New objective", blockedBy: undefined };
    expect(resolveWorkItemDetail(fallback, list, { workItem: full, dataRequestId: 1 }, 2)).toMatchObject({
      objective: list.objective,
      blockedBy: undefined,
    });
  });

  it("resolves navigation against newer detail and never reuses its old full fields", () => {
    const full = { ...fallback, objective: "Updated objective", todoList: [], workRefs: [], planArtifact: undefined };
    expect(resolveWorkItemDetail(fallback, fallback, { workItem: full, dataRequestId: 2 }, 1)).toMatchObject(full);
  });

  it("keeps newer list status without overwriting detail-owned plan/todo", () => {
    const list = { ...fallback, objective: "New summary", state: "completed", current: false };
    const full = { ...fallback, todoList: [{ text: "New todo", state: "completed" as const }], resultSummary: "Old result" };
    expect(resolveWorkItemDetail(fallback, list, { workItem: full, dataRequestId: 1 }, 2)).toMatchObject({
      objective: "New summary", state: "completed", current: false, todoList: full.todoList, resultSummary: "Old result",
    });
  });

  it("keeps removed items inspectable but does not resurrect their current flag", () => {
    expect(resolveWorkItemDetail(fallback, undefined, { workItem: fallback, dataRequestId: 2 }, 3)).toMatchObject({
      objective: fallback.objective, current: false,
    });
  });
});
