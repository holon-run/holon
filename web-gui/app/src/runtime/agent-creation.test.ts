// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { BOOTSTRAP_STEPS, bootstrapProgress, type AgentBootstrap, type AgentCreateResponse } from "./agent-creation";

const mocks = vi.hoisted(() => ({
  create: vi.fn(), detail: vi.fn(), job: vi.fn(), refresh: vi.fn(),
}));
vi.mock("./client", async (original) => ({
  ...await original<typeof import("./client")>(),
  createRuntimeClient: () => ({
    createAgentFromTemplate: mocks.create, getAgentIdentity: mocks.detail, getJob: mocks.job,
  }),
}));

import { RuntimeHttpError } from "./client";
import { useRuntimeStore } from "./runtime-store";

const initial = useRuntimeStore.getState();
const identity = { agent_id: "new-agent", incarnation: 3, status: "active" as const };
function bootstrap(status: "pending" | "succeeded" | "failed" = "succeeded"): AgentBootstrap {
  return {
    status: status === "succeeded" ? "ready" : "degraded",
    ...Object.fromEntries(BOOTSTRAP_STEPS.map((key) => [key, {
      status, attempts: 1, updated_at: "2026-10-25T00:00:00Z",
      last_error: status === "failed" ? "private SQL /home/secret credential" : null,
    }])),
  } as AgentBootstrap;
}
function creation(stage: AgentCreateResponse["receipt"]["stage"] = "provisioning"): AgentCreateResponse {
  return {
    identity, receipt: {
      receipt_id: "receipt-3", agent_id: identity.agent_id, stage, job_id: "job-3",
      bootstrap: bootstrap(stage === "bootstrapped" ? "succeeded" : stage === "degraded" ? "failed" : "pending"),
    },
  };
}
const create = () => useRuntimeStore.getState().createAgentFromTemplate("new-agent", "template", "Display name");
const progress = () => useRuntimeStore.getState().agentCreations["new-agent"];

beforeEach(() => {
  vi.useFakeTimers();
  vi.resetAllMocks();
  mocks.create.mockResolvedValue(creation());
  mocks.detail.mockResolvedValue({ identity, bootstrap: bootstrap() });
  mocks.job.mockResolvedValue({ status: "completed" });
  mocks.refresh.mockResolvedValue(undefined);
  useRuntimeStore.setState({ ...initial, agentCreations: {}, refreshBootstrap: mocks.refresh, templateCatalogError: "catalog-only" }, true);
});
afterEach(async () => {
  // Invalidates any delayed observer from this test.
  mocks.refresh.mockResolvedValue(undefined);
  await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
  vi.clearAllTimers();
  vi.useRealTimers();
  useRuntimeStore.setState(initial, true);
});

describe("agent creation boundaries", () => {
  it("returns accepted before polling and preserves identity/name and catalog diagnostics", async () => {
    expect(await create()).toMatchObject({ kind: "accepted", creation: { identity } });
    expect(mocks.create).toHaveBeenCalledWith("new-agent", "template", "Display name");
    expect(mocks.job).not.toHaveBeenCalled();
    expect(progress().status).toBe("initializing");
    expect(useRuntimeStore.getState().templateCatalogError).toBe("catalog-only");
    await vi.advanceTimersByTimeAsync(1000);
    expect(progress().status).toBe("ready");
    expect(mocks.detail).toHaveBeenCalledWith("new-agent");
  });

  it("accepts degraded receipts without throwing or suggesting another POST", async () => {
    mocks.create.mockResolvedValue(creation("degraded"));
    expect(await create()).toMatchObject({ kind: "accepted" });
    expect(progress().status).toBe("degraded");
    expect(useRuntimeStore.getState().templateCatalogError).toBe("catalog-only");
  });

  it("accepts bootstrapped receipts as completed initialization without another POST", async () => {
    mocks.create.mockResolvedValue(creation("bootstrapped"));
    expect(await create()).toMatchObject({ kind: "accepted", creation: { identity } });
    expect(progress().status).toBe("ready");
    expect(mocks.create).toHaveBeenCalledTimes(1);
  });

  it("does not accept bootstrap summary status as a receipt stage", async () => {
    const response = creation("bootstrapped");
    mocks.create.mockResolvedValue({ ...response, receipt: { ...response.receipt, stage: "ready" } });
    expect(await create()).toEqual({ kind: "uncertain" });
    expect(progress()).toBeUndefined();
    expect(mocks.create).toHaveBeenCalledTimes(1);
  });

  it("keeps roster refresh failure separate from accepted creation and successful initialization", async () => {
    mocks.refresh.mockRejectedValue(new Error("roster offline"));
    expect(await create()).toMatchObject({ kind: "accepted" });
    await vi.advanceTimersByTimeAsync(1000);
    expect(progress().status).toBe("ready");
  });

  it("uses durable failed steps, not unsafe job errors, for initialization failure", async () => {
    mocks.job.mockResolvedValue({ status: "failed", error: "private path /home/secret SQL" });
    mocks.detail.mockResolvedValue({ identity, bootstrap: bootstrap("failed") });
    await create();
    await vi.advanceTimersByTimeAsync(1000);
    expect(progress().status).toBe("degraded");
    expect(useRuntimeStore.getState().templateCatalogError).toBe("catalog-only");
    expect(mocks.create).toHaveBeenCalledTimes(1);
  });

  it.each(["missing", "network"] as const)("recovers a %s volatile job through durable detail", async (failure) => {
    mocks.job.mockRejectedValue(failure === "missing"
      ? new RuntimeHttpError("GET", "/jobs/job-3", 404) : new TypeError("offline"));
    await create();
    await vi.advanceTimersByTimeAsync(1000);
    expect(progress().status).toBe("ready");
  });

  it("leaves the status unconfirmed when job and durable observation are unavailable, then allows read-only recovery", async () => {
    mocks.job.mockRejectedValue(new Error("offline"));
    mocks.detail.mockRejectedValue(new Error("offline"));
    await create();
    await vi.advanceTimersByTimeAsync(1000);
    expect(progress().status).toBe("uncertain");
    mocks.detail.mockResolvedValue({ identity, bootstrap: bootstrap() });
    await useRuntimeStore.getState().refreshAgentCreation("new-agent");
    expect(progress().status).toBe("ready");
    expect(mocks.create).toHaveBeenCalledTimes(1);
  });

  it("does not equate an expired poll or completed job with ready initialization", async () => {
    mocks.job.mockResolvedValue({ status: "running" });
    mocks.detail.mockResolvedValue({ identity, bootstrap: bootstrap("pending") });
    await create();
    await vi.advanceTimersByTimeAsync(121000);
    expect(progress().status).toBe("uncertain");
    expect(mocks.create).toHaveBeenCalledTimes(1);
  });

  it("checks durable detail when a provisioning receipt has no job", async () => {
    const response = creation();
    delete response.receipt.job_id;
    mocks.create.mockResolvedValue(response);
    await create();
    expect(mocks.detail).toHaveBeenCalledTimes(1);
    expect(progress().status).toBe("ready");
  });

  it.each([
    [409, "already_exists", "conflict"],
    [409, "deletion_incomplete", "deletion_incomplete"],
    [400, "agent_name_invalid", "invalid"],
    [401, undefined, "unauthorized"],
    [403, undefined, "forbidden"],
    [404, undefined, "not_found"],
    [429, undefined, "busy"],
    [503, undefined, "busy"],
    [500, "private_error_code", "server"],
    [409, "some_other_conflict", "unexpected"],
  ] as const)("returns a safe operation error for HTTP %s/%s", async (status, code, problem) => {
    mocks.create.mockRejectedValue(new RuntimeHttpError("POST", "/create", status, "secret SQL /home/private token", code));
    expect(await create()).toEqual({ kind: "rejected", status, problem });
    expect(mocks.detail).not.toHaveBeenCalled();
    expect(useRuntimeStore.getState().templateCatalogError).toBe("catalog-only");
  });

  it.each([new Error("private plain error"), new TypeError("network"), new SyntaxError("bad receipt")])(
    "treats receipt-less exceptions as uncertain without exposing raw text", async (error) => {
      mocks.create.mockRejectedValue(error);
      expect(await create()).toEqual({ kind: "uncertain" });
      expect(progress()).toBeUndefined();
    },
  );

  it("does not accept a malformed or mismatched identity", async () => {
    mocks.create.mockResolvedValue({ receipt: {} });
    expect(await create()).toEqual({ kind: "uncertain" });
    mocks.create.mockResolvedValue({ ...creation(), identity: { ...identity, agent_id: "other" } });
    expect(await create()).toEqual({ kind: "uncertain" });
  });

  it("discards a POST resolved after a connection generation change", async () => {
    let resolve!: (value: AgentCreateResponse) => void;
    mocks.create.mockReturnValue(new Promise((done) => { resolve = done; }));
    const pending = create();
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    resolve(creation());
    expect(await pending).toEqual({ kind: "stale" });
    expect(progress()).toBeUndefined();
  });

  it("fences late bootstrap results after connection replacement", async () => {
    let resolve!: (value: unknown) => void;
    mocks.job.mockReturnValue(new Promise((done) => { resolve = done; }));
    await create();
    await vi.advanceTimersByTimeAsync(1000);
    await useRuntimeStore.getState().setRuntimeConnection({ mode: "local" });
    resolve({ status: "completed" });
    await vi.advanceTimersByTimeAsync(0);
    expect(progress()).toBeUndefined();
    expect(mocks.detail).not.toHaveBeenCalled();
  });

  it("does not attach an old receipt to a recreated identity", async () => {
    mocks.detail.mockResolvedValue({ identity: { ...identity, incarnation: 4 }, bootstrap: bootstrap() });
    await create();
    await vi.advanceTimersByTimeAsync(1000);
    expect(progress()).toBeUndefined();
  });

  it("only offers a verified active identity for opening; 409 itself never means success", async () => {
    expect(await useRuntimeStore.getState().lookupCreatedAgent("new-agent")).toEqual({ kind: "found", identity });
    mocks.detail.mockResolvedValue({ identity: { ...identity, status: "deleted" } });
    expect(await useRuntimeStore.getState().lookupCreatedAgent("new-agent")).toEqual({ kind: "missing" });
    mocks.detail.mockRejectedValue(new RuntimeHttpError("GET", "/detail", 404));
    expect(await useRuntimeStore.getState().lookupCreatedAgent("new-agent")).toEqual({ kind: "missing" });
    mocks.detail.mockRejectedValue(new Error("offline"));
    expect(await useRuntimeStore.getState().lookupCreatedAgent("new-agent")).toEqual({ kind: "unavailable" });
    expect(mocks.create).not.toHaveBeenCalled();
  });

  it("does not call pending bootstrap degraded merely because summary status is degraded", () => {
    expect(bootstrapProgress(bootstrap("pending"))).toBe("initializing");
  });

  it.each(["pending", "error"] as const)("fences an older manual %s observation after the automatic observer reaches ready", async (outcome) => {
    let resolve!: (value: unknown) => void;
    let reject!: (error: Error) => void;
    await create();
    mocks.detail.mockImplementationOnce(() => new Promise((done, fail) => { resolve = done; reject = fail; }));
    const manual = useRuntimeStore.getState().refreshAgentCreation("new-agent");
    await vi.advanceTimersByTimeAsync(1000);
    expect(progress().status).toBe("ready");
    if (outcome === "pending") resolve({ identity, bootstrap: bootstrap("pending") });
    else reject(new Error("old request offline"));
    await manual;
    expect(progress().status).toBe("ready");
  });

  it("fences POST and identity lookup results after scope changes without a client replacement", async () => {
    let resolveCreate!: (value: AgentCreateResponse) => void;
    let resolveDetail!: (value: unknown) => void;
    mocks.create.mockReturnValue(new Promise((done) => { resolveCreate = done; }));
    mocks.detail.mockReturnValue(new Promise((done) => { resolveDetail = done; }));
    const pendingCreate = create();
    const pendingLookup = useRuntimeStore.getState().lookupCreatedAgent("new-agent");
    useRuntimeStore.setState((state) => ({ agentCreationContext: state.agentCreationContext + 1, agentCreations: {} }));
    resolveCreate(creation());
    resolveDetail({ identity, bootstrap: bootstrap() });
    expect(await pendingCreate).toEqual({ kind: "stale" });
    expect(await pendingLookup).toEqual({ kind: "stale" });
    expect(progress()).toBeUndefined();
  });

  it("retires the creation entry and keeps late observations from restoring it", async () => {
    await create();
    expect(progress()).toBeDefined();
    useRuntimeStore.getState().dismissAgentCreation("new-agent");
    expect(progress()).toBeUndefined();
    // A pending observation must not resurrect the retired banner.
    await vi.advanceTimersByTimeAsync(1000);
    expect(progress()).toBeUndefined();
    expect(mocks.create).toHaveBeenCalledTimes(1);
  });
});
