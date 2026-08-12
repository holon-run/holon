import { afterEach, describe, expect, it, vi } from "vitest";

import { useRuntimeStore, type AgentSessionState } from "./runtime-store";
import { createSessionProjectionState } from "./session-projection";

class MemoryStorage implements Storage {
  private readonly items = new Map<string, string>();

  get length() {
    return this.items.size;
  }

  clear(): void {
    this.items.clear();
  }

  getItem(key: string): string | null {
    return this.items.get(key) ?? null;
  }

  key(index: number): string | null {
    return Array.from(this.items.keys())[index] ?? null;
  }

  removeItem(key: string): void {
    this.items.delete(key);
  }

  setItem(key: string, value: string): void {
    this.items.set(key, value);
  }
}

function sessionState(overrides: Partial<AgentSessionState> = {}): AgentSessionState {
  return {
    ...createSessionProjectionState(),
    loading: false,
    semanticHistoryByDisplayLevel: {},
    targetEventLoading: false,
    liveStatus: "idle",
    cacheStatus: "unchecked",
    contentStatus: "unknown",
    syncStatus: "idle",
    sendingPrompt: false,
    detail: null,
    workItemDetailsById: {},
    taskDetailsById: {},
    toolExecutionDetailsById: {},
    ...overrides,
  };
}

describe("global event stream recovery", () => {
  afterEach(() => {
    useRuntimeStore.getState().stopGlobalEventStream();
    useRuntimeStore.getState().unregisterAgentForEvents("agent-a");
    useRuntimeStore.setState({
      sessionsByAgentId: {},
      globalStreamStatus: "idle",
      selectedAgentId: "",
    });
    vi.unstubAllGlobals();
  });

  it("does not report streaming until the subscribed agent backfill completes", async () => {
    const localStorage = new MemoryStorage();
    const sessionStorage = new MemoryStorage();
    vi.stubGlobal("window", {
      localStorage,
      sessionStorage,
      setTimeout,
      clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
    let resolveBackfill!: (response: Response) => void;
    const backfill = new Promise<Response>((resolve) => {
      resolveBackfill = resolve;
    });
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) return Promise.resolve(jsonResponse({}));
      if (url.pathname.endsWith("/agents/list")) return Promise.resolve(jsonResponse([]));
      if (url.pathname.endsWith("/events/stream")) {
        return Promise.resolve(new Response(new ReadableStream<Uint8Array>({
          start(controller) {
            init?.signal?.addEventListener("abort", () => controller.close());
          },
        }), {
          status: 200,
          headers: { "content-type": "text/event-stream" },
        }));
      }
      if (url.pathname.endsWith("/agents/agent-a/events")) return backfill;
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();
    useRuntimeStore.setState({
      sessionsByAgentId: {
        "agent-a": sessionState({
          eventLogEpoch: "epoch-1",
          eventsBySeq: {
            1: {
              agent_id: "agent-a",
              event_seq: 1,
              event_log_epoch: "epoch-1",
              type: "legacy_event",
              payload: {},
            },
            5: {
              agent_id: "agent-a",
              event_seq: 5,
              event_log_epoch: "epoch-1",
              type: "legacy_event",
              payload: {},
            },
          },
          eventSeqs: [1, 5],
          newestSeq: 5,
          gaps: [{ afterSeq: 1, beforeSeq: 5 }],
        }),
      },
    });

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("catching_up");
    });
    const eventRequest = fetchMock.mock.calls
      .map(([input]) => new URL(String(input), "http://localhost"))
      .find((url) => url.pathname.endsWith("/agents/agent-a/events"));
    expect(eventRequest?.searchParams.get("after_seq")).toBe("1");

    resolveBackfill(jsonResponse({
      events: [2, 3, 4, 5].map((eventSeq) => ({
        id: `event-${eventSeq}`,
        event_seq: eventSeq,
        event_log_epoch: "epoch-1",
        ts: "2026-08-09T00:00:00Z",
        agent_id: "agent-a",
        type: "legacy_event",
        payload: {},
      })),
      event_log_epoch: "epoch-1",
      has_older: false,
      has_newer: false,
      order: "asc",
      limit: 100,
    }));
    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("streaming");
    });
  });

  it("retries baseline initialization before declaring recovery complete", async () => {
    const localStorage = new MemoryStorage();
    const sessionStorage = new MemoryStorage();
    const retryCallbacks: Array<() => void> = [];
    vi.stubGlobal("window", {
      localStorage,
      sessionStorage,
      setTimeout: (callback: () => void, delay?: number) => {
        if (delay === 1_000) retryCallbacks.push(callback);
        return retryCallbacks.length;
      },
      clearTimeout: () => undefined,
      location: { hostname: "localhost", protocol: "http:" },
    });
    let baselineAttempts = 0;
    const fetchMock = vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(String(input), "http://localhost");
      if (url.pathname.endsWith("/handshake")) return Promise.resolve(jsonResponse({}));
      if (url.pathname.endsWith("/agents/list")) return Promise.resolve(jsonResponse([]));
      if (url.pathname.endsWith("/events/stream")) {
        return Promise.resolve(new Response(new ReadableStream<Uint8Array>({
          start(controller) {
            init?.signal?.addEventListener("abort", () => controller.close());
          },
        }), {
          status: 200,
          headers: { "content-type": "text/event-stream" },
        }));
      }
      if (url.pathname.endsWith("/agents/agent-a/events")) {
        if (url.searchParams.get("order") === "desc") {
          baselineAttempts += 1;
          if (baselineAttempts === 1) return Promise.reject(new Error("baseline unavailable"));
          return Promise.resolve(jsonResponse({
            events: [{
              id: "event-1",
              event_seq: 1,
              event_log_epoch: "epoch-1",
              ts: "2026-08-10T00:00:00Z",
              agent_id: "agent-a",
              type: "legacy_event",
              payload: {},
            }],
            event_log_epoch: "epoch-1",
            has_older: false,
            has_newer: false,
            order: "desc",
            limit: 100,
          }));
        }
        return Promise.resolve(jsonResponse({
          events: [],
          event_log_epoch: "epoch-1",
          has_older: false,
          has_newer: false,
          order: "asc",
          limit: 100,
        }));
      }
      throw new Error(`Unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    fetchMock.mockClear();

    useRuntimeStore.getState().registerAgentForEvents("agent-a");

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().sessionsByAgentId["agent-a"]).toMatchObject({
        liveStatus: "recovering",
        syncError: "baseline unavailable",
        syncRetryAttempt: 1,
      });
    });
    expect(useRuntimeStore.getState().globalStreamStatus).toBe("catching_up");
    expect(retryCallbacks).toHaveLength(1);

    retryCallbacks.shift()?.();

    await vi.waitFor(() => {
      expect(useRuntimeStore.getState().globalStreamStatus).toBe("streaming");
    });
    expect(baselineAttempts).toBe(2);
    expect(useRuntimeStore.getState().sessionsByAgentId["agent-a"]).toMatchObject({
      eventSeqs: [1],
      liveStatus: "streaming",
      syncError: undefined,
      syncRetryAttempt: undefined,
    });
  });
});

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}
