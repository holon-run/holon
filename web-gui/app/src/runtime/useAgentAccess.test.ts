// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  identity: vi.fn(),
  state: vi.fn(),
  connection: { mode: "local", baseUrl: "/api" },
}));
vi.mock("./runtime-store", () => ({
  getRuntimeConnectionConfig: () => ({ mode: "local" }),
  useRuntimeStore: (select: (state: unknown) => unknown) => select({ bootstrap: { connection: mocks.connection, agents: [] } }),
}));
vi.mock("./client", async (original) => ({
  ...await original<typeof import("./client")>(),
  createRuntimeClient: () => ({ getAgentIdentity: mocks.identity, getAgentState: mocks.state }),
}));

import { RuntimeHttpError } from "./client";
import { useAgentAccess } from "./useAgentAccess";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root;
let container: HTMLDivElement;
let current: ReturnType<typeof useAgentAccess>;
function childDetail(supervision: Record<string, unknown> = {}, resolution = "resolved") {
  return {
    identity: { agent_id: "private-child", incarnation: 1, status: "active", parent_agent_id: "legacy-parent", delegated_from_task_id: "legacy-task" },
    canonical_relations: {
      agent_id: "private-child", resolution,
      supervision: {
        supervision_id: "supervision-1", supervisor_agent_id: "parent", child_agent_id: "private-child",
        delegated_from_task_id: "task-1", state: "active", ...supervision,
      },
    },
  };
}
function withChild(detail: unknown, parent: unknown = { identity: { agent_id: "parent", status: "active" } }) {
  mocks.identity.mockImplementation(async (id: string) => id === "parent" ? parent : detail);
}
function Probe({ id, incarnation }: { id: string; incarnation?: number }) {
  current = useAgentAccess(id, incarnation);
  return null;
}
async function render(id = "private-child", incarnation?: number) {
  await act(async () => { root.render(createElement(Probe, { id, incarnation })); });
}
beforeEach(() => {
  vi.useFakeTimers();
  mocks.identity.mockReset();
  withChild(childDetail());
  mocks.state.mockReset().mockResolvedValue({ id: "private-child", incarnation: 1, acceptsInput: true });
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});
afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.useRealTimers();
});

describe("route-selected agent admission", () => {
  it("loads a private child without adding it to the public roster", async () => {
    await render();
    expect(current.status).toBe("available");
    expect(current.agent?.id).toBe("private-child");
    expect(current.parentAgentId).toBe("parent");
    expect(current.parentTaskId).toBe("task-1");
    expect(mocks.identity).toHaveBeenCalledWith("parent");
  });

  it.each([
    { lineage_parent_agent_id: "creator" },
    { lineage_parent_agent_id: "default", parent_agent_id: "default" },
    { profile_preset: "private_child", parent_agent_id: "creator" },
    { ownership: "parent_supervised", parent_agent_id: "creator" },
  ])("does not infer navigation from legacy identity fields: %j", async (identity) => {
    mocks.identity.mockResolvedValue({ identity: { agent_id: "private-child", incarnation: 1, status: "active", ...identity } });
    await render();
    expect(current.status).toBe("available");
    expect(current.parentAgentId).toBeUndefined();
    expect(current.parentTaskId).toBeUndefined();
    expect(mocks.identity).toHaveBeenCalledTimes(1);
  });

  it.each(["ambiguous", "contradictory", "missing_evidence"])("hides an unresolved %s supervision", async (resolution) => {
    withChild(childDetail({}, resolution));
    await render();
    expect(current.parentAgentId).toBeUndefined();
    expect(mocks.identity).toHaveBeenCalledTimes(1);
  });

  it.each([
    { state: "closed" },
    { child_agent_id: "other-child" },
    { supervisor_agent_id: "private-child" },
    { supervisor_agent_id: "default" },
  ])("rejects invalid navigation without probing a legacy route: %j", async (supervision) => {
    withChild(childDetail(supervision));
    await render();
    expect(current.parentAgentId).toBeUndefined();
    expect(mocks.identity).toHaveBeenCalledTimes(1);
  });

  it.each(["deleting", "deleted"])("hides a %s parent without disabling child input", async (status) => {
    withChild(childDetail(), { identity: { agent_id: "parent", status } });
    await render();
    expect(current.parentAgentId).toBeUndefined();
    expect(current.agent?.acceptsInput).toBe(true);
  });

  it.each([401, 403, 404, 500])("ignores parent HTTP %s independently of the child", async (code) => {
    mocks.identity.mockImplementation(async (id: string) => {
      if (id === "parent") throw new RuntimeHttpError("GET", "/parent/detail", code);
      return childDetail();
    });
    await render();
    expect(current.status).toBe("available");
    expect(current.parentAgentId).toBeUndefined();
    expect(current.agent?.acceptsInput).toBe(true);
  });

  it("does not substitute a differently resolved identity for the supervisor", async () => {
    withChild(childDetail(), { identity: { agent_id: "somewhere-else", status: "active" } });
    await render();
    expect(current.parentAgentId).toBeUndefined();
  });

  it("does not hold child admission behind a slow parent lookup", async () => {
    let resolveParent!: (value: unknown) => void;
    mocks.identity.mockImplementation(async (id: string) => id === "parent"
      ? new Promise((resolve) => { resolveParent = resolve; }) : childDetail());
    await render();
    expect(current.status).toBe("available");
    expect(current.agent?.acceptsInput).toBe(true);
    expect(current.parentAgentId).toBeUndefined();
    await act(async () => resolveParent({ identity: { agent_id: "parent", status: "active" } }));
    expect(current.parentAgentId).toBe("parent");
  });

  it("removes the return link when the parent disappears during polling", async () => {
    await render();
    withChild(childDetail(), { identity: { agent_id: "parent", status: "deleted" } });
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(current.parentAgentId).toBeUndefined();
    expect(current.status).toBe("available");
  });

  it("keeps cleanup navigation for a deleted child without reading its state", async () => {
    const detail = childDetail({ state: "cleanup_required" });
    detail.identity.status = "deleted";
    withChild(detail);
    await render();
    expect(current.status).toBe("deleted");
    expect(current.parentAgentId).toBe("parent");
    expect(mocks.state).not.toHaveBeenCalled();
  });

  it("discards a late parent lookup after navigating to an independent agent", async () => {
    let resolveParent!: (value: unknown) => void;
    mocks.identity.mockImplementation(async (id: string) => id === "parent"
      ? new Promise((resolve) => { resolveParent = resolve; })
      : id === "private-child" ? childDetail() : { identity: { agent_id: id, incarnation: 1, status: "active" } });
    await render();
    await render("independent");
    await act(async () => resolveParent({ identity: { agent_id: "parent", status: "active" } }));
    expect(current.parentAgentId).toBeUndefined();
    expect(current.status).toBe("available");
  });

  it.each(["deleting", "deleted"] as const)("keeps %s distinct without reading runtime state", async (status) => {
    mocks.identity.mockResolvedValue({ identity: { status, incarnation: 1 } });
    await render();
    expect(current.status).toBe(status);
    expect(mocks.state).not.toHaveBeenCalled();
  });

  it.each([[401, "unauthorized"], [403, "unauthorized"], [404, "missing"], [500, "error"]] as const)(
    "preserves HTTP %s as %s instead of redirecting", async (code, status) => {
      mocks.identity.mockRejectedValue(new RuntimeHttpError("GET", "/detail", code));
      await render();
      expect(current.status).toBe(status);
      expect(current.agent).toBeUndefined();
    },
  );

  it("rejects a stale task incarnation before opening the state or conversation", async () => {
    await render("private-child", 2);
    expect(current.status).toBe("replaced");
    expect(current.parentAgentId).toBeUndefined();
    expect(mocks.state).not.toHaveBeenCalled();
    expect(mocks.identity).not.toHaveBeenCalledWith("parent");
  });

  it("pins a bare URL identity across polling and retry", async () => {
    await render();
    mocks.identity.mockResolvedValue({ identity: { status: "active", incarnation: 2 } });
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(current.status).toBe("replaced");
    await act(async () => current.retry());
    expect(current.status).toBe("replaced");
    expect(mocks.state).toHaveBeenCalledTimes(1);
  });

  it("rejects an identity change between detail and state", async () => {
    mocks.state.mockResolvedValue({ id: "private-child", incarnation: 2 });
    await render();
    expect(current.status).toBe("replaced");
    expect(current.parentAgentId).toBeUndefined();
  });

  it("does not allow a late previous agent response to replace the route target", async () => {
    let resolve!: (value: unknown) => void;
    mocks.identity.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    await render("old");
    await render("new");
    expect(current.status).toBe("available");
    await act(async () => resolve({ identity: { status: "deleted", incarnation: 1 } }));
    expect(current.status).toBe("available");
    expect(mocks.state).toHaveBeenCalledTimes(1);
  });
});
