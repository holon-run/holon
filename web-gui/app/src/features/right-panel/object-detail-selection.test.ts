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
