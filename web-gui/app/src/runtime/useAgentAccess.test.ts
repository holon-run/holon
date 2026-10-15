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
function Probe({ id, incarnation }: { id: string; incarnation?: number }) {
  current = useAgentAccess(id, incarnation);
  return null;
}
async function render(id = "private-child", incarnation?: number) {
  await act(async () => { root.render(createElement(Probe, { id, incarnation })); });
}
beforeEach(() => {
  vi.useFakeTimers();
  mocks.identity.mockReset().mockResolvedValue({
    identity: { agent_id: "private-child", incarnation: 1, status: "active", parent_agent_id: "parent", delegated_from_task_id: "task-1" },
  });
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
    expect(mocks.state).not.toHaveBeenCalled();
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
