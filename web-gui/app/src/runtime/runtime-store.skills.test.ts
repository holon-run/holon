import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useRuntimeStore } from "./runtime-store";
import type { AgentSummary } from "./types";

const initialState = useRuntimeStore.getState();
const json = (body: unknown) => new Response(JSON.stringify(body), {
  headers: { "content-type": "application/json" },
});
const skill = (name: string) => ({
  skill_id: name, root_id: "library", skill_dir: name, name,
  description: name, path: `/skills/${name}`, scope: "user_global",
});
const agent = (id: string, incarnation = 1): AgentSummary => ({
  id, incarnation, badge: "", profile: "", lifecycle: "idle", focusSummary: "",
  workspace: "", attention: "", model: "", footer: "", subtitle: "", lastBrief: "",
  lastTurnTime: "", pending: 0, activeTaskCount: 0, waitingCount: 0,
  posture: "", postureReason: "",
});

let read: (agentId?: string) => Response | Promise<Response>;
let write: (path: string) => Response | Promise<Response>;
let fetchMock: ReturnType<typeof vi.fn>;

function reads(agentId?: string): number {
  const suffix = agentId ? `/agents/${agentId}/skills` : "/skills/catalog";
  return fetchMock.mock.calls.filter(([input, init]) =>
    String(input).endsWith(suffix) && (!init?.method || init.method === "GET")).length;
}

function setIncarnation(incarnation: number): void {
  useRuntimeStore.setState((state) => ({
    bootstrap: { ...state.bootstrap, agents: [agent("agent-a", incarnation), agent("agent-b")] },
  }));
}

async function refreshBoth(force = false): Promise<void> {
  await Promise.all([
    useRuntimeStore.getState().refreshSkillCatalog({ force }),
    useRuntimeStore.getState().refreshAgentSkillCatalog("agent-a", { force }),
  ]);
}

describe("Skill catalog freshness and invalidation", () => {
  beforeEach(async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-31T08:00:00Z"));
    const storage = new Map<string, string>();
    const memoryStorage = {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => { storage.set(key, value); },
      removeItem: (key: string) => { storage.delete(key); },
    };
    vi.stubGlobal("window", {
      localStorage: memoryStorage, sessionStorage: memoryStorage, setTimeout, clearTimeout,
      location: { hostname: "localhost", protocol: "http:" },
    });
    read = (agentId) => json(agentId ? { skills: [skill("enabled")] } : { catalog: [skill("available")] });
    write = () => json({ ok: true });
    fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      if (init?.method && init.method !== "GET") return write(path);
      if (path.endsWith("/skills/catalog")) return read();
      const match = path.match(/\/agents\/([^/]+)\/skills$/);
      if (match) return read(match[1]);
      if (path.endsWith("/handshake")) return json({ capabilities: [
        "agents.roster-snapshot.v1", "agents.projection-snapshot.v1",
        "events.projection-effect.v1", "briefs.atomic-created-event.v1",
      ] });
      if (path.endsWith("/agents/snapshot")) return json({
        contract_version: 1, runtime_id: "runtime-1", event_log_epoch: "epoch-1",
        visibility_scope_id: "scope-1", agents: [],
      });
      if (path.endsWith("/agents/list")) return json([]);
      if (path.endsWith("/jobs/job-1")) return json({ job: { id: "job-1", status: "completed" } });
      throw new Error(`Unexpected request: ${path}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    useRuntimeStore.setState({
      selectedAgentId: "agent-a",
      discovery: { mode: "authoritative", freshness: "fresh", retryAttempt: 0, identity: {
        runtimeId: "runtime-1", visibilityScopeId: "scope-1", eventLogEpoch: "epoch-1",
      } },
    });
    setIncarnation(1);
  });

  afterEach(() => {
    vi.clearAllTimers();
    useRuntimeStore.setState(initialState, true);
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("revalidates both catalogs at 60 seconds, including an empty catalog, without polling", async () => {
    read = (agentId) => json(agentId ? { skills: [] } : { catalog: [] });
    await refreshBoth();
    await vi.advanceTimersByTimeAsync(59_999);
    await refreshBoth();
    expect([reads(), reads("agent-a")]).toEqual([1, 1]);
    await vi.advanceTimersByTimeAsync(1);
    await refreshBoth();
    expect([reads(), reads("agent-a")]).toEqual([2, 2]);
    await vi.advanceTimersByTimeAsync(180_000);
    expect([reads(), reads("agent-a")]).toEqual([2, 2]);
  });

  it("gives a recreated Agent its own retry budget after both incarnations fail", async () => {
    read = () => new Response("offline", { status: 503 });
    await useRuntimeStore.getState().refreshAgentSkillCatalog("agent-a");
    setIncarnation(2);
    await useRuntimeStore.getState().refreshAgentSkillCatalog("agent-a");
    expect(reads("agent-a")).toBe(2);
    read = () => json({ skills: [skill("recovered")] });
    await vi.advanceTimersByTimeAsync(2_500);
    expect(reads("agent-a")).toBe(3);
    const catalog = useRuntimeStore.getState().agentSkillCatalogByAgentId["agent-a"];
    expect(catalog.incarnation).toBe(2);
    expect(catalog.stale).toBe(false);
    expect(catalog.catalog[0].name).toBe("recovered");
  });

  it("bypasses a fresh TTL on manual refresh and shares concurrent reads", async () => {
    await refreshBoth();
    let finish!: (response: Response) => void;
    read = () => new Promise<Response>((resolve) => { finish = resolve; });
    const first = useRuntimeStore.getState().refreshSkillCatalog();
    const second = useRuntimeStore.getState().refreshSkillCatalog({ force: false });
    expect(first).toBe(second);
    await Promise.resolve();
    finish(json({ catalog: [] }));
    await Promise.all([first, second]);
    expect(reads()).toBe(2);
    read = (agentId) => json(agentId ? { skills: [] } : { catalog: [] });
    await useRuntimeStore.getState().refreshAgentSkillCatalog("agent-a");
    expect(reads("agent-a")).toBe(2);
  });

  it("keeps successful data and timestamps stale on failure, then recovers on entry", async () => {
    await refreshBoth();
    const previous = useRuntimeStore.getState();
    read = () => new Response("offline", { status: 503 });
    await refreshBoth(true);
    let state = useRuntimeStore.getState();
    expect(state.skillCatalog).toMatchObject({
      catalog: previous.skillCatalog.catalog, lastSuccessAt: previous.skillCatalog.lastSuccessAt, stale: true,
    });
    expect(state.agentSkillCatalogByAgentId["agent-a"]).toMatchObject({
      catalog: previous.agentSkillCatalogByAgentId["agent-a"].catalog,
      lastSuccessAt: previous.agentSkillCatalogByAgentId["agent-a"].lastSuccessAt, stale: true,
    });
    expect(state.agentSkillCatalogErrorByAgentId["agent-a"]).toContain("503");
    read = (agentId) => json(agentId ? { skills: [] } : { catalog: [] });
    await refreshBoth();
    state = useRuntimeStore.getState();
    expect(state.skillCatalog.stale).toBe(false);
    expect(state.agentSkillCatalogErrorByAgentId["agent-a"]).toBeUndefined();
    await vi.advanceTimersByTimeAsync(90_000);
    expect([reads(), reads("agent-a")]).toEqual([3, 3]);
  });

  it("does not reuse a recreated Agent's catalog or merge its old in-flight response", async () => {
    await refreshBoth();
    let finish!: (response: Response) => void;
    read = () => new Promise<Response>((resolve) => { finish = resolve; });
    const old = useRuntimeStore.getState().refreshAgentSkillCatalog("agent-a");
    await Promise.resolve();
    setIncarnation(2);
    read = () => json({ skills: [skill("new-incarnation")] });
    const fresh = useRuntimeStore.getState().refreshAgentSkillCatalog("agent-a", { force: false });
    expect(useRuntimeStore.getState().agentSkillCatalogByAgentId["agent-a"].catalog).toEqual([]);
    await fresh;
    finish(json({ skills: [skill("old-incarnation")] }));
    await old;
    expect(useRuntimeStore.getState().agentSkillCatalogByAgentId["agent-a"]).toMatchObject({
      incarnation: 2, catalog: [expect.objectContaining({ name: "new-incarnation" })],
    });
  });

  it("shares Agent reads and discards queued reads when their identity is already replaced", async () => {
    const first = useRuntimeStore.getState().refreshAgentSkillCatalog("agent-a");
    const second = useRuntimeStore.getState().refreshAgentSkillCatalog("agent-a", { force: false });
    expect(first).toBe(second);
    setIncarnation(2);
    await Promise.all([first, second]);
    expect(reads("agent-a")).toBe(0);
    expect(useRuntimeStore.getState().agentSkillCatalogLoadingByAgentId["agent-a"]).toBe(false);
    await useRuntimeStore.getState().refreshAgentSkillCatalog("agent-a", { force: false });
    expect(reads("agent-a")).toBe(1);
  });

  it("revalidates both catalogs when the authorization context changes", async () => {
    await refreshBoth();
    useRuntimeStore.setState({
      currentUser: { userId: "another-user", authMethod: "session" }, currentUserLoaded: true,
    });
    await refreshBoth();
    expect([reads(), reads("agent-a")]).toEqual([2, 2]);
  });

  it("fences global reads and TTL reuse by discovery identity and client generation", async () => {
    await refreshBoth();
    let finish!: (response: Response) => void;
    read = () => new Promise<Response>((resolve) => { finish = resolve; });
    const old = useRuntimeStore.getState().refreshSkillCatalog();
    await Promise.resolve();
    useRuntimeStore.setState((state) => ({ discovery: {
      ...state.discovery, identity: { ...state.discovery.identity!, eventLogEpoch: "epoch-2" },
    } }));
    read = () => json({ catalog: [skill("new-epoch")] });
    await useRuntimeStore.getState().refreshSkillCatalog({ force: false });
    finish(json({ catalog: [skill("old-epoch")] }));
    await old;
    expect(useRuntimeStore.getState().skillCatalog.catalog[0].name).toBe("new-epoch");
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    await useRuntimeStore.getState().refreshSkillCatalog({ force: false });
    expect(reads()).toBe(4);
  });

  it.each(["enableAgentSkill", "disableAgentSkill"] as const)(
    "%s invalidates both affected catalogs, not another Agent's cache", async (method) => {
      await refreshBoth();
      await useRuntimeStore.getState().refreshAgentSkillCatalog("agent-b");
      const other = useRuntimeStore.getState().agentSkillCatalogByAgentId["agent-b"];
      expect(await useRuntimeStore.getState()[method]("agent-a", "enabled")).toBe(true);
      expect([reads(), reads("agent-a"), reads("agent-b")]).toEqual([2, 2, 1]);
      expect(useRuntimeStore.getState().agentSkillCatalogByAgentId["agent-b"]).toBe(other);
    },
  );

  it("supersedes reads started before a successful mutation rather than joining them", async () => {
    await refreshBoth();
    let finish!: (response: Response) => void;
    read = () => new Promise<Response>((resolve) => { finish = resolve; });
    const old = useRuntimeStore.getState().refreshAgentSkillCatalog("agent-a");
    await Promise.resolve();
    read = (agentId) => json(agentId ? { skills: [skill("after-write")] } : { catalog: [] });
    expect(await useRuntimeStore.getState().enableAgentSkill("agent-a", "after-write")).toBe(true);
    finish(json({ skills: [skill("before-write")] }));
    await old;
    expect(reads("agent-a")).toBe(3);
    expect(useRuntimeStore.getState().agentSkillCatalogByAgentId["agent-a"].catalog[0].name).toBe("after-write");
  });

  it("retains durable mutation success when its follow-up reads fail", async () => {
    await refreshBoth();
    read = () => new Response("offline", { status: 503 });
    expect(await useRuntimeStore.getState().disableAgentSkill("agent-a", "enabled")).toBe(true);
    expect(useRuntimeStore.getState().skillCatalog.stale).toBe(true);
    expect(useRuntimeStore.getState().agentSkillCatalogByAgentId["agent-a"].stale).toBe(true);
  });

  it("does not refresh a recreated Agent after an old incarnation's write completes", async () => {
    await refreshBoth();
    let finish!: (response: Response) => void;
    write = () => new Promise<Response>((resolve) => { finish = resolve; });
    const old = useRuntimeStore.getState().enableAgentSkill("agent-a", "enabled");
    setIncarnation(2);
    finish(json({ ok: true }));
    expect(await old).toBe(false);
    expect([reads(), reads("agent-a")]).toEqual([1, 1]);
  });

  it("does not invalidate catalogs on a rejected mutation", async () => {
    await refreshBoth();
    write = () => new Response("rejected", { status: 403 });
    expect(await useRuntimeStore.getState().enableAgentSkill("agent-a", "enabled")).toBe(false);
    expect([reads(), reads("agent-a")]).toEqual([1, 1]);
    expect(useRuntimeStore.getState().skillCatalog.stale).toBe(false);
  });

  it.each(["enableAgentSkill", "disableAgentSkill"] as const)(
    "%s discards an old incarnation's write failure", async (method) => {
      await refreshBoth();
      let finish!: (response: Response) => void;
      write = () => new Promise<Response>((resolve) => { finish = resolve; });
      const old = useRuntimeStore.getState()[method]("agent-a", "enabled");
      setIncarnation(2);
      await useRuntimeStore.getState().refreshAgentSkillCatalog("agent-a", { force: false });
      finish(new Response("old-incarnation rejected", { status: 403 }));
      expect(await old).toBe(false);
      expect(useRuntimeStore.getState().agentSkillCatalogErrorByAgentId["agent-a"]).toBeUndefined();
      expect(useRuntimeStore.getState().agentSkillCatalogLoadingByAgentId["agent-a"]).toBe(false);
      expect(useRuntimeStore.getState().agentSkillCatalogByAgentId["agent-a"].incarnation).toBe(2);
    },
  );

  it.each(["removeSkillFromCatalog", "checkSkillCatalog"] as const)(
    "%s refreshes the visible catalogs and invalidates other linked catalogs", async (method) => {
      await refreshBoth();
      await useRuntimeStore.getState().refreshAgentSkillCatalog("agent-b");
      expect(await useRuntimeStore.getState()[method]("enabled")).toBe(true);
      expect([reads(), reads("agent-a"), reads("agent-b")]).toEqual([2, 2, 1]);
      expect(useRuntimeStore.getState().agentSkillCatalogByAgentId["agent-b"].stale).toBe(true);
    },
  );

  it.each(["add", "update"] as const)("invalidates both catalogs after an asynchronous %s job completes", async (kind) => {
    await refreshBoth();
    write = () => json({ job: { id: "job-1", status: "queued" } });
    const result = kind === "add"
      ? await useRuntimeStore.getState().addSkillToCatalog({ kind: "named", name: "enabled" })
      : await useRuntimeStore.getState().updateSkillCatalog("enabled");
    expect(result).toBe(true);
    expect([reads(), reads("agent-a")]).toEqual([1, 1]);
    await vi.advanceTimersByTimeAsync(1_000);
    expect([reads(), reads("agent-a")]).toEqual([2, 2]);
  });
});
