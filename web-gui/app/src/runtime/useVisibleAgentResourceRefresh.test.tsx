// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const conversation = vi.hoisted(() => ({ head: 1, listeners: new Set<() => void>() }));
vi.mock("./conversation-scope-store", () => ({
  conversationScopeSnapshot: () => ({
    status: { kind: "ready" },
    view: { reset_reason: null, scope: "scope", through_seq: conversation.head },
  }),
  subscribeConversationScope: (_key: string, listener: () => void) => {
    conversation.listeners.add(listener);
    return () => conversation.listeners.delete(listener);
  },
}));

import { RuntimeHttpError } from "./client";
import {
  registerSelectedAgentRefreshContext,
  setResumeAdmissionStatus,
  useResumeAdmission,
  useSelectedAgentRefreshContext,
} from "./selected-agent-refresh-context";
import { useVisibleAgentResourceRefresh } from "./useVisibleAgentResourceRefresh";

let root: Root;
let host: HTMLDivElement;
let hidden = false;
const read = vi.fn();
const errors = vi.fn();
let result: ReturnType<typeof useVisibleAgentResourceRefresh<string[]>>;

function Harness({ visible = true }: { visible?: boolean }) {
  result = useVisibleAgentResourceRefresh<string[]>("a", "timers", visible, read);
  return <div>{result.state?.data?.join(",")}</div>;
}
function admit(incarnation = 1, key = `scope-${incarnation}`) {
  return registerSelectedAgentRefreshContext({ agentId: "a", incarnation, key, scopeKey: "scope", onAccessError: errors });
}
async function render(visible = true) {
  await act(async () => root.render(<Harness visible={visible} />));
  await advance(0);
}
async function advance(ms: number) {
  await act(async () => { await vi.advanceTimersByTimeAsync(ms); });
}
async function hint() {
  await act(async () => {
    conversation.head++;
    conversation.listeners.forEach((notify) => notify());
  });
}
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => hidden ? "hidden" : "visible" });
  hidden = false;
  conversation.head = 1;
  conversation.listeners.clear();
  read.mockReset().mockResolvedValue(["initial"]);
  errors.mockReset();
  useSelectedAgentRefreshContext.setState({ current: null });
  useResumeAdmission.setState({ status: { pending: false, token: 0 } });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  useSelectedAgentRefreshContext.setState({ current: null });
  useResumeAdmission.setState({ status: { pending: false, token: 0 } });
  Reflect.deleteProperty(document, "visibilityState");
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("visible overview resource refresh", () => {
  it("requires route admission and visible section ownership", async () => {
    await render();
    expect(read).not.toHaveBeenCalled();
    await act(async () => { admit(); });
    await advance(0);
    expect(read).toHaveBeenCalledExactlyOnceWith("a", { incarnation: 1, signal: expect.any(AbortSignal) });
    await render(false);
    await hint();
    await advance(60_000);
    expect(read).toHaveBeenCalledTimes(1);
    await render();
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("merges hints, enforces the 5s lower bound, and falls back after 30s", async () => {
    admit();
    await render();
    await hint();
    await hint();
    await advance(4_999);
    expect(read).toHaveBeenCalledTimes(1);
    await advance(1);
    expect(read).toHaveBeenCalledTimes(2);
    await advance(30_000);
    expect(read).toHaveBeenCalledTimes(3);
  });

  it("retains stale data on failure, respects Retry-After, and retries explicitly", async () => {
    admit();
    await render();
    read.mockRejectedValueOnce(new RuntimeHttpError("GET", "/timers", 429, "busy", undefined, undefined, undefined, 60));
    await act(async () => result.refresh());
    await advance(0);
    expect(result.state).toMatchObject({ data: ["initial"], stale: true, error: expect.stringContaining("busy"), loading: false });
    await act(async () => result.refresh());
    await advance(59_999);
    expect(read).toHaveBeenCalledTimes(2);
    await advance(1);
    expect(read).toHaveBeenCalledTimes(3);
    expect(result.state?.stale).toBe(false);
  });

  it("single-flights a dirty in-flight read and hides old data on incarnation change", async () => {
    admit();
    await render();
    let resolve!: (value: string[]) => void;
    read.mockImplementationOnce(() => new Promise<string[]>((done) => { resolve = done; }));
    await act(async () => result.refresh());
    await advance(0);
    const oldSignal = read.mock.calls[1][1].signal as AbortSignal;
    await hint();
    await advance(10_000);
    expect(read).toHaveBeenCalledTimes(2);
    await act(async () => { admit(2); });
    expect(host.textContent).toBe("");
    expect(oldSignal.aborted).toBe(true);
    read.mockResolvedValue(["new"]);
    await advance(0);
    await act(async () => resolve(["obsolete"]));
    expect(host.textContent).toBe("new");
  });

  it("restarts a superseded generation read on renewed admission while keeping last known data", async () => {
    admit();
    read.mockResolvedValueOnce(["known"]);
    await render();
    let resolve!: (value: string[]) => void;
    read.mockImplementationOnce(() => new Promise<string[]>((done) => { resolve = done; }));
    await act(async () => result.refresh());
    await advance(0);
    const obsoleteSignal = read.mock.calls.at(-1)?.[1].signal as AbortSignal;
    read.mockResolvedValueOnce(["current"]);
    await act(async () => { admit(); });
    expect(obsoleteSignal.aborted).toBe(true);
    expect(host.textContent).toBe("known");
    await advance(0);
    expect(host.textContent).toBe("current");
    await act(async () => resolve(["obsolete"]));
    expect(host.textContent).toBe("current");
    expect(read).toHaveBeenCalledTimes(3);
  });

  it("pauses hidden/offline and coalesces visibility, focus and online recovery", async () => {
    admit();
    await render();
    await act(async () => {
      hidden = true;
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await hint();
    await advance(120_000);
    expect(read).toHaveBeenCalledTimes(1);
    await act(async () => {
      hidden = false;
      document.dispatchEvent(new Event("visibilitychange"));
      window.dispatchEvent(new Event("focus"));
      window.dispatchEvent(new Event("online"));
    });
    await advance(0);
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("holds automatic reads behind pending resume reconciliation and reads once after settle", async () => {
    admit();
    await render();
    hidden = true;
    await act(async () => { document.dispatchEvent(new Event("visibilitychange")); });
    await advance(120_000);
    expect(read).toHaveBeenCalledTimes(1);
    await act(async () => { setResumeAdmissionStatus({ pending: true, token: 1 }); });
    hidden = false;
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      window.dispatchEvent(new Event("focus"));
      window.dispatchEvent(new Event("online"));
      window.dispatchEvent(new Event("pageshow"));
    });
    await advance(2_000);
    expect(read).toHaveBeenCalledTimes(1);
    await act(async () => { admit(1); });
    await hint();
    await advance(2_000);
    expect(read).toHaveBeenCalledTimes(1);
    await act(async () => { setResumeAdmissionStatus({ pending: false, token: 2 }); });
    await advance(0);
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("keeps failures local but forwards Agent access rejection", async () => {
    admit();
    read.mockRejectedValueOnce(new RuntimeHttpError("GET", "/timers", 403, "forbidden"));
    await render();
    expect(errors).toHaveBeenCalledTimes(1);
    await hint();
    await advance(60_000);
    expect(read).toHaveBeenCalledTimes(1);
    await act(async () => result.refresh());
    await advance(0);
    expect(read).toHaveBeenCalledTimes(2);
  });

  it("does not let old context cleanup revoke the replacement context", () => {
    const unregister = admit();
    admit(2);
    unregister();
    expect(useSelectedAgentRefreshContext.getState().current?.incarnation).toBe(2);
  });
});
