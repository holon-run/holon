import { beforeEach, describe, expect, it, vi } from "vitest";

import { AgentSessionRepository, isSessionCacheContextCurrent } from "./agent-session-repository";
import { cacheClearRemoteSessions, cacheDeleteSession } from "./idb-cache";
import type { RuntimeConnectionConfig } from "./types";

vi.mock("./idb-cache", () => ({
  cacheClearRemoteSessions: vi.fn(async () => undefined),
  cacheDeleteSession: vi.fn(async () => undefined),
}));

function createHarness() {
  let config: RuntimeConnectionConfig = { mode: "local" };
  let generation = 1;
  const repository = new AgentSessionRepository({
    getConnectionConfig: () => config,
    getGeneration: () => generation,
  });
  return {
    repository,
    advanceGeneration: () => { generation += 1; },
    changeRemote: () => { config = { mode: "remote", baseUrl: "https://other.example/" }; },
  };
}

beforeEach(() => vi.clearAllMocks());

describe("legacy session cache cleanup", () => {
  it("only depends on remote identity and generation, without an events client", async () => {
    const { repository } = createHarness();
    repository.initializeCache();
    repository.initializeCache();
    expect(cacheClearRemoteSessions).toHaveBeenCalledOnce();
    expect(cacheClearRemoteSessions).toHaveBeenCalledWith("local", expect.any(Function));
    await repository.deleteCachedSession("agent-a");
    expect(cacheDeleteSession).toHaveBeenCalledWith("local", "agent-a");
  });

  it("rejects obsolete cleanup after a generation change", () => {
    const harness = createHarness();
    harness.repository.initializeCache();
    const isCurrent = vi.mocked(cacheClearRemoteSessions).mock.calls[0]![1]!;
    expect(isCurrent()).toBe(true);
    harness.advanceGeneration();
    expect(isCurrent()).toBe(false);
  });

  it("rejects an old remote and permits initialization for the new one", () => {
    const harness = createHarness();
    harness.repository.initializeCache();
    const oldIsCurrent = vi.mocked(cacheClearRemoteSessions).mock.calls[0]![1]!;
    harness.changeRemote();
    expect(oldIsCurrent()).toBe(false);
    harness.repository.switchRemote();
    harness.repository.initializeCache();
    expect(cacheClearRemoteSessions).toHaveBeenLastCalledWith("https://other.example", expect.any(Function));
    expect(vi.mocked(cacheClearRemoteSessions).mock.calls[1]![1]!()).toBe(true);
  });
});

describe("isSessionCacheContextCurrent", () => {
  it("requires both the remote and generation to match", () => {
    const context = { remoteKey: "local", generation: 2 };
    expect(isSessionCacheContextCurrent(context, "local", 2)).toBe(true);
    expect(isSessionCacheContextCurrent(context, "other", 2)).toBe(false);
    expect(isSessionCacheContextCurrent(context, "local", 3)).toBe(false);
  });
});
