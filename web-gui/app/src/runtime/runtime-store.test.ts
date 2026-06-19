import { afterEach, describe, expect, it, vi } from "vitest";

import {
  canUseRemoteRuntimeConnections,
  isLoopbackWebHostname,
  readStoredRemoteConnectionProfiles,
  readStoredRuntimeConnectionConfig,
  writeStoredRuntimeConnectionConfig,
} from "./runtime-store";

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

function installWindow(localStorage: Storage, sessionStorage: Storage, hostname = "localhost") {
  vi.stubGlobal("window", {
    clearTimeout: () => undefined,
    location: { hostname },
    localStorage,
    sessionStorage,
  });
}

describe("runtime connection storage", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("keeps active runtime connections isolated per window session", () => {
    const sharedLocalStorage = new MemoryStorage();
    const remoteWindowSession = new MemoryStorage();
    const localWindowSession = new MemoryStorage();

    installWindow(sharedLocalStorage, remoteWindowSession);
    writeStoredRuntimeConnectionConfig({
      mode: "remote",
      baseUrl: "http://remote.example:7878/",
      token: "remote-token",
    });

    installWindow(sharedLocalStorage, localWindowSession);
    expect(readStoredRuntimeConnectionConfig()).toEqual({ mode: "local" });
    writeStoredRuntimeConnectionConfig({ mode: "local" });

    installWindow(sharedLocalStorage, remoteWindowSession);
    expect(readStoredRuntimeConnectionConfig()).toEqual({
      mode: "remote",
      baseUrl: "http://remote.example:7878",
      token: "remote-token",
    });

    installWindow(sharedLocalStorage, localWindowSession);
    expect(readStoredRuntimeConnectionConfig()).toEqual({ mode: "local" });
  });

  it("retains saved remote tokens without making new windows remote by default", () => {
    const sharedLocalStorage = new MemoryStorage();
    const firstWindowSession = new MemoryStorage();
    const secondWindowSession = new MemoryStorage();

    installWindow(sharedLocalStorage, firstWindowSession);
    writeStoredRuntimeConnectionConfig({
      mode: "remote",
      baseUrl: "http://remote.example:7878",
      token: "saved-token",
    });

    installWindow(sharedLocalStorage, secondWindowSession);
    expect(readStoredRuntimeConnectionConfig()).toEqual({ mode: "local" });
    writeStoredRuntimeConnectionConfig({ mode: "remote", baseUrl: "http://remote.example:7878" });

    expect(readStoredRuntimeConnectionConfig()).toEqual({
      mode: "remote",
      baseUrl: "http://remote.example:7878",
      token: "saved-token",
    });
  });

  it("keeps same-origin runtime tokens in the active window session", () => {
    const sharedLocalStorage = new MemoryStorage();
    const windowSession = new MemoryStorage();

    installWindow(sharedLocalStorage, windowSession, "100.92.113.47");
    writeStoredRuntimeConnectionConfig({ mode: "local", token: "same-origin-token" });

    expect(readStoredRuntimeConnectionConfig()).toEqual({
      mode: "local",
      token: "same-origin-token",
    });
    expect(readStoredRemoteConnectionProfiles()).toEqual([]);
  });

  it("detects loopback page origins as eligible for remote runtime connections", () => {
    expect(isLoopbackWebHostname("localhost")).toBe(true);
    expect(isLoopbackWebHostname("127.0.0.1")).toBe(true);
    expect(isLoopbackWebHostname("127.42.0.9")).toBe(true);
    expect(isLoopbackWebHostname("::1")).toBe(true);
    expect(isLoopbackWebHostname("100.92.113.47")).toBe(false);
    expect(isLoopbackWebHostname("holon.example.test")).toBe(false);
  });

  it("forces same-origin local mode on non-loopback embedded pages", () => {
    const sharedLocalStorage = new MemoryStorage();
    const remoteWindowSession = new MemoryStorage();

    installWindow(sharedLocalStorage, remoteWindowSession, "100.92.113.47");
    expect(canUseRemoteRuntimeConnections()).toBe(false);

    writeStoredRuntimeConnectionConfig({
      mode: "remote",
      baseUrl: "http://127.0.0.1:7878",
      token: "saved-token",
    });

    expect(readStoredRuntimeConnectionConfig()).toEqual({ mode: "local" });
    expect(readStoredRemoteConnectionProfiles()).toEqual([]);
  });
});

describe("roster activity unread state", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("hydrates persisted roster activity per remote key", async () => {
    const sharedLocalStorage = new MemoryStorage();
    installWindow(sharedLocalStorage, new MemoryStorage());
    sharedLocalStorage.setItem(
      "holon.webGui.rosterActivityByRemote.v1",
      JSON.stringify({
        local: {
          localAgent: { unreadCount: 2, lastUnreadSeq: 12, lastReadSeq: 7, briefAt: "2026-01-01T00:00:00.000Z" },
        },
        "http://remote.example:7878": {
          remoteAgent: { unreadCount: 4, lastUnreadSeq: 20 },
        },
      }),
    );

    const { readStoredRosterActivity } = await import("./runtime-store");

    expect(readStoredRosterActivity("local")).toEqual({
      localAgent: { unreadCount: 2, lastUnreadSeq: 12, lastReadSeq: 7, briefAt: "2026-01-01T00:00:00.000Z" },
    });
    expect(readStoredRosterActivity("http://remote.example:7878")).toEqual({
      remoteAgent: { unreadCount: 4, lastUnreadSeq: 20 },
    });
  });

  it("counts unread brief and non-operator message events once by seq", async () => {
    const { touchRosterActivityFromEvent } = await import("./runtime-store");
    const afterBrief = touchRosterActivityFromEvent(
      {},
      "agent-a",
      { agent_id: "agent-a", event_seq: 10, ts: "2026-01-01T00:00:00.000Z", type: "brief_created", payload: {} },
      "agent-b",
    );
    const afterDuplicate = touchRosterActivityFromEvent(
      afterBrief,
      "agent-a",
      { agent_id: "agent-a", event_seq: 10, ts: "2026-01-01T00:00:01.000Z", type: "brief_created", payload: {} },
      "agent-b",
    );
    const afterAgentMessage = touchRosterActivityFromEvent(
      afterDuplicate,
      "agent-a",
      {
        agent_id: "agent-a",
        event_seq: 11,
        ts: "2026-01-01T00:00:02.000Z",
        type: "message_enqueued",
        payload: { origin: { kind: "agent" } },
      },
      "agent-b",
    );

    expect(afterAgentMessage["agent-a"]).toMatchObject({ unreadCount: 2, lastUnreadSeq: 11 });
  });

  it("does not count unread for the currently open agent or operator messages", async () => {
    const { touchRosterActivityFromEvent } = await import("./runtime-store");
    const afterSelectedBrief = touchRosterActivityFromEvent(
      {},
      "agent-a",
      { agent_id: "agent-a", event_seq: 10, ts: "2026-01-01T00:00:00.000Z", type: "brief_created", payload: {} },
      "agent-a",
    );
    const afterOperatorMessage = touchRosterActivityFromEvent(
      afterSelectedBrief,
      "agent-a",
      {
        agent_id: "agent-a",
        event_seq: 11,
        ts: "2026-01-01T00:00:01.000Z",
        type: "message_enqueued",
        payload: { origin: { kind: "operator" }, created_at: "2026-01-01T00:00:01.000Z" },
      },
      "agent-b",
    );

    expect(afterOperatorMessage["agent-a"]?.unreadCount).toBeUndefined();
    expect(afterOperatorMessage["agent-a"]?.operatorAt).toBe("2026-01-01T00:00:01.000Z");
  });
});
