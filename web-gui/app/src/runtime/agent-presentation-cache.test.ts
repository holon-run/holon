import { IDBFactory, IDBDatabase } from "fake-indexeddb";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AgentSummary } from "./types";
import {
  AGENT_PRESENTATION_CACHE_SCHEMA_VERSION, _resetDbPromise, cacheClearAll, cacheClearRemote, cacheGetAgentPresentations,
  cachePutAgentPresentations, type CachedAgentPresentations,
} from "./idb-cache";
import {
  AGENT_PRESENTATION_CACHE_MAX_AGE_MS, AGENT_PRESENTATION_CACHE_MAX_AGENTS,
  AGENT_PRESENTATION_CACHE_MAX_BYTES, agentPresentationCacheKey,
  readCachedAgents, removeCachedAgent, writeCachedAgents,
} from "./agent-presentation-cache";

beforeEach(() => { _resetDbPromise(); globalThis.indexedDB = new IDBFactory(); });
afterEach(() => { _resetDbPromise(); vi.restoreAllMocks(); });

const user = { authMethod: "token", userId: "alice" };
const key = agentPresentationCacheKey("local", user)!;
const agent = (id = "agent"): AgentSummary => ({
  id, name: "Display name", incarnation: 2, badge: "A", badgeHue: 32, profile: "developer",
  lifecycle: "idle", focusSummary: "Current focus", workspace: "Workspace", attention: "",
  model: "model", footer: "Footer", subtitle: "Subtitle", lastBrief: "Last result",
  lastTurnTime: "", pending: 1, activeTaskCount: 0, waitingCount: 0, posture: "ready",
  postureReason: "", acceptsInput: true, canRename: true,
});
const entry = (overrides: Partial<CachedAgentPresentations> = {}): CachedAgentPresentations => ({
  remoteKey: key, schemaVersion: AGENT_PRESENTATION_CACHE_SCHEMA_VERSION, cachedAt: Date.now(), agents: [agent()], ...overrides,
});

describe("agent presentation cache", () => {
  it("requires a user and separates remote, user id and authentication method", async () => {
    expect(agentPresentationCacheKey("local", undefined)).toBeUndefined();
    expect(agentPresentationCacheKey("local", { ...user, userId: "" })).toBeUndefined();
    expect(agentPresentationCacheKey("local", { ...user, authMethod: "" })).toBeUndefined();
    await writeCachedAgents(key, [agent()]);
    for (const other of [
      agentPresentationCacheKey("remote", user)!,
      agentPresentationCacheKey("local", { ...user, userId: "bob" })!,
      agentPresentationCacheKey("local", { ...user, authMethod: "session" })!,
    ]) {
      expect(other).not.toBe(key);
      expect(await readCachedAgents(other)).toBeUndefined();
    }
    expect(agentPresentationCacheKey("local", { authMethod: "a:b", userId: "c" }))
      .not.toBe(agentPresentationCacheKey("local", { authMethod: "a", userId: "b:c" }));
  });

  it("keeps deep-linked agents separate from the roster and other identity buckets", async () => {
    const single = agentPresentationCacheKey("local", user, "private")!;
    expect(agentPresentationCacheKey("local", user, undefined)).toBe(key);
    await writeCachedAgents(key, [agent("public")]);
    await writeCachedAgents(single, [agent("private")]);
    expect((await readCachedAgents(key))?.map((value) => value.id)).toEqual(["public"]);
    expect((await readCachedAgents(single))?.map((value) => value.id)).toEqual(["private"]);
    for (const other of [
      agentPresentationCacheKey("local", user, "another")!,
      agentPresentationCacheKey("remote", user, "private")!,
      agentPresentationCacheKey("local", { ...user, userId: "bob" }, "private")!,
      agentPresentationCacheKey("local", { ...user, authMethod: "session" }, "private")!,
    ]) {
      expect(other).not.toBe(single);
      expect(await readCachedAgents(other)).toBeUndefined();
    }
    expect(agentPresentationCacheKey("local", { authMethod: "a", userId: "b:c" }, "d"))
      .not.toBe(agentPresentationCacheKey("local", { authMethod: "a", userId: "b" }, "c:d"));
    await Promise.all([removeCachedAgent(key, "private"), removeCachedAgent(single, "private")]);
    expect((await readCachedAgents(key))?.map((value) => value.id)).toEqual(["public"]);
    expect(await readCachedAgents(single)).toEqual([]);
  });

  it("persists display and lifecycle hints without restoring privileged capabilities", async () => {
    const live = {
      ...agent(), tasks: [{ id: "private-task" }], workItems: [{ id: "private-work" }],
      attachedWorkspaces: [{ id: "private-workspace" }], currentRunId: "private-run",
      currentWork: { id: "private-focus" }, waits: [{ secret: true }],
      workspaceSummary: { secret: true }, unknownAuthorization: true,
    } as unknown as AgentSummary;
    await writeCachedAgents(key, [live]);
    const expected = { ...agent(), canRename: false };
    expect(await readCachedAgents(key)).toEqual([expected]);
    expect((await cacheGetAgentPresentations(key) as CachedAgentPresentations).agents).toEqual([expected]);
    // Even a tampered entry cannot restore rename eligibility or deep execution data.
    await cachePutAgentPresentations(entry({ agents: [live] }));
    expect(await readCachedAgents(key)).toEqual([expected]);
    expect(live.acceptsInput).toBe(true);
  });

  it.each([true, false, undefined])("preserves the input lifecycle hint: %s", async (acceptsInput) => {
    await writeCachedAgents(key, [{ ...agent(), acceptsInput }]);
    expect((await readCachedAgents(key))?.[0].acceptsInput).toBe(acceptsInput);
  });

  it("restores a legitimate empty list and filters retiring/deleted agents", async () => {
    const agents = [agent(), { ...agent("retiring"), lifecycle: "deleting" }, { ...agent("gone"), lifecycle: "deleted" }];
    await writeCachedAgents(key, agents);
    expect((await readCachedAgents(key))?.map((value) => value.id)).toEqual(["agent"]);
    await cachePutAgentPresentations(entry({ agents }));
    expect((await readCachedAgents(key))?.map((value) => value.id)).toEqual(["agent"]);
    await writeCachedAgents(key, []);
    expect(await readCachedAgents(key)).toEqual([]);
  });

  it.each([
    { id: "" }, { id: 1 }, { badge: null }, { profile: undefined }, { lifecycle: "" },
    { incarnation: undefined }, { incarnation: 0 }, { incarnation: -1 }, { incarnation: 1.5 },
    { incarnation: Number.MAX_SAFE_INTEGER + 1 }, { incarnation: Infinity },
    { pending: NaN }, { activeTaskCount: Infinity }, { waitingCount: -1 }, { waitingCount: "2" },
    { badgeHue: NaN }, { badgeHue: "3" }, { name: {} }, { modelSource: "unexpected" },
    { badgeTone: "unexpected" }, { acceptsInput: "false" },
  ])("rejects malformed display summaries: %j", async (overrides) => {
    await cachePutAgentPresentations(entry({ agents: [{ ...agent(), ...overrides }] }));
    expect(await readCachedAgents(key)).toBeUndefined();
  });

  it.each([
    { schemaVersion: 0 }, { schemaVersion: 1 }, { cachedAt: NaN }, { cachedAt: Infinity }, { cachedAt: 0 },
    { cachedAt: Date.now() + 60_000 }, { cachedAt: Date.now() - AGENT_PRESENTATION_CACHE_MAX_AGE_MS - 60_000 },
    { agents: undefined }, { agents: {} }, { agents: [null] }, { agents: [agent(), agent()] },
  ])("ignores incompatible, expired or corrupted envelopes: %j", async (overrides) => {
    await cachePutAgentPresentations(entry(overrides as Partial<CachedAgentPresentations>));
    expect(await readCachedAgents(key)).toBeUndefined();
  });

  it("bounds written lists and rejects over-limit stored lists", async () => {
    const agents = Array.from({ length: AGENT_PRESENTATION_CACHE_MAX_AGENTS + 1 }, (_, index) => agent(`agent-${index}`));
    await writeCachedAgents(key, agents);
    expect(await readCachedAgents(key)).toHaveLength(AGENT_PRESENTATION_CACHE_MAX_AGENTS);
    await cachePutAgentPresentations(entry({ agents }));
    expect(await readCachedAgents(key)).toBeUndefined();
    const huge = { ...agent("huge"), lastBrief: "x".repeat(AGENT_PRESENTATION_CACHE_MAX_BYTES) };
    await cachePutAgentPresentations(entry({ agents: [huge] }));
    expect(await readCachedAgents(key)).toBeUndefined();
    await writeCachedAgents(key, [huge, agent()]);
    expect((await readCachedAgents(key))?.map((value) => value.id)).toEqual(["agent"]);
    const medium = Array.from({ length: 20 }, (_, index) => ({
      ...agent(`medium-${index}`), lastBrief: "x".repeat(100_000),
    }));
    await writeCachedAgents(key, medium);
    const persisted = await cacheGetAgentPresentations(key);
    expect(JSON.stringify(persisted).length * 2).toBeLessThanOrEqual(AGENT_PRESENTATION_CACHE_MAX_BYTES);
    expect((await readCachedAgents(key))!.length).toBeGreaterThan(0);
    expect((await readCachedAgents(key))!.length).toBeLessThan(medium.length);
  });

  it("removes agents atomically without changing the original cache age", async () => {
    const cachedAt = Date.now() - 1000;
    await cachePutAgentPresentations(entry({ cachedAt, agents: [agent("a"), agent("b"), agent("c")] }));
    await Promise.all([removeCachedAgent(key, "a"), removeCachedAgent(key, "b")]);
    expect((await readCachedAgents(key))?.map((value) => value.id)).toEqual(["c"]);
    expect((await cacheGetAgentPresentations(key) as CachedAgentPresentations).cachedAt).toBe(cachedAt);
    await Promise.all([writeCachedAgents(key, [agent("a"), agent("new")]), removeCachedAgent(key, "a")]);
    expect((await readCachedAgents(key))?.map((value) => value.id)).toEqual(["new"]);
    await removeCachedAgent("missing", "a");
    expect(await readCachedAgents("missing")).toBeUndefined();
  });

  it("clears every identity namespace of one remote, then all remotes on logout", async () => {
    const secondUser = agentPresentationCacheKey("local", { ...user, userId: "bob" })!;
    const otherRemote = agentPresentationCacheKey("other", user)!;
    const single = agentPresentationCacheKey("local", user, "private")!;
    const secondUserSingle = agentPresentationCacheKey("local", { ...user, userId: "bob" }, "private")!;
    const otherRemoteSingle = agentPresentationCacheKey("other", user, "private")!;
    for (const scope of [key, secondUser, otherRemote, single, secondUserSingle, otherRemoteSingle]) {
      await writeCachedAgents(scope, [agent()]);
    }
    await cacheClearRemote("local");
    expect(await readCachedAgents(key)).toBeUndefined();
    expect(await readCachedAgents(secondUser)).toBeUndefined();
    expect(await readCachedAgents(single)).toBeUndefined();
    expect(await readCachedAgents(secondUserSingle)).toBeUndefined();
    expect(await readCachedAgents(otherRemote)).toHaveLength(1);
    expect(await readCachedAgents(otherRemoteSingle)).toHaveLength(1);
    await cacheClearAll();
    expect(await readCachedAgents(otherRemote)).toBeUndefined();
    expect(await readCachedAgents(otherRemoteSingle)).toBeUndefined();
  });

  it("does not resurrect writes queued before logout while the database is opening", async () => {
    const write = writeCachedAgents(key, [agent()]);
    const remove = removeCachedAgent(key, "missing");
    const read = readCachedAgents(key);
    const clear = cacheClearAll();
    await Promise.all([write, remove, clear]);
    expect(await read).toBeUndefined();
    expect(await readCachedAgents(key)).toBeUndefined();
    await writeCachedAgents(key, [agent()]);
    expect(await readCachedAgents(key)).toHaveLength(1);
  });

  it("invalidates pending reads and writes even when the database is already open", async () => {
    await writeCachedAgents(key, [agent()]);
    const read = readCachedAgents(key);
    const write = writeCachedAgents(key, [agent("new")]);
    const clear = cacheClearRemote("local");
    await Promise.all([write, clear]);
    expect(await read).toBeUndefined();
    expect(await readCachedAgents(key)).toBeUndefined();
  });

  it("falls back without IndexedDB and when opening fails", async () => {
    vi.stubGlobal("indexedDB", undefined);
    try {
      await writeCachedAgents(key, [agent()]);
      expect(await readCachedAgents(key)).toBeUndefined();
      await removeCachedAgent(key, "agent");
    } finally { vi.unstubAllGlobals(); _resetDbPromise(); }
    vi.spyOn(indexedDB, "open").mockImplementation(() => { throw new Error("disabled"); });
    await writeCachedAgents(key, [agent()]);
    expect(await readCachedAgents(key)).toBeUndefined();
  });

  it("falls back when transactions fail or a persisted payload cannot be serialized", async () => {
    await writeCachedAgents(key, [agent()]);
    const transaction = vi.spyOn(IDBDatabase.prototype, "transaction")
      .mockImplementation(() => { throw new Error("storage unavailable"); });
    expect(await readCachedAgents(key)).toBeUndefined();
    await writeCachedAgents(key, [agent("new")]);
    await removeCachedAgent(key, "agent");
    transaction.mockRestore();
    expect((await readCachedAgents(key))?.map((value) => value.id)).toEqual(["agent"]);
    const cyclic: Record<string, unknown> = {};
    cyclic.self = cyclic;
    await cachePutAgentPresentations(entry({ agents: [{ ...agent(), unknown: cyclic }] }));
    expect(await readCachedAgents(key)).toBeUndefined();
  });
});
