import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  AGENT_AVATAR_CAPABILITY, AgentAvatarResources, avatarResourceRequest,
} from "./agent-avatar-resources";

const capabilities = [AGENT_AVATAR_CAPABILITY];
const png = (bytes = 4) => new Blob([new Uint8Array(bytes)], { type: "image/png" });
const request = (id = "a") => avatarResourceRequest(id, "neutral");
const limits = { entries: 128, bytes: 1024, imageBytes: 128, concurrency: 4 };
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
let createUrl = vi.fn<typeof URL.createObjectURL>();
let revokeUrl = vi.fn<typeof URL.revokeObjectURL>();

beforeEach(() => {
  let sequence = 0;
  createUrl = vi.fn(() => `blob:avatar-${++sequence}`);
  revokeUrl = vi.fn();
  vi.spyOn(URL, "createObjectURL").mockImplementation(createUrl);
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(revokeUrl);
});
afterEach(() => vi.restoreAllMocks());

describe("authenticated avatar resources", () => {
  it("requires the exact negotiated capability, not an endpoint guess", async () => {
    const resources = new AgentAvatarResources();
    const load = vi.fn(async () => ({ blob: png() }));
    resources.configure(load, ["agent_avatar", "agent_avatar_v2"]);
    resources.retain(request(), vi.fn(), resources.getEpoch());
    await Promise.resolve();
    expect(load).not.toHaveBeenCalled();
    resources.configure(load, capabilities);
    resources.retain(request(), vi.fn(), resources.getEpoch());
    await vi.waitFor(() => expect(load).toHaveBeenCalledOnce());
    resources.reset();
  });

  it("coalesces in-flight consumers and revokes only the final object URL reference", async () => {
    const resources = new AgentAvatarResources();
    const pending = deferred<{ blob: Blob }>();
    const load = vi.fn(() => pending.promise);
    resources.configure(load, capabilities);
    const first = resources.retain(request(), vi.fn(), resources.getEpoch());
    const second = resources.retain(request(), vi.fn(), resources.getEpoch());
    await vi.waitFor(() => expect(load).toHaveBeenCalledOnce());
    pending.resolve({ blob: png() });
    await vi.waitFor(() => expect(resources.peek(request(), resources.getEpoch())).toBe("blob:avatar-1"));
    first();
    expect(revokeUrl).not.toHaveBeenCalled();
    second();
    second();
    expect(revokeUrl).toHaveBeenCalledExactlyOnceWith("blob:avatar-1");
    expect(resources.peek(request(), resources.getEpoch())).toBeUndefined();
    const again = resources.retain(request(), vi.fn(), resources.getEpoch());
    expect(resources.peek(request(), resources.getEpoch())).toBe("blob:avatar-2");
    expect(load).toHaveBeenCalledOnce();
    again();
    resources.reset();
  });

  it("keys by identity, version, size, and expression without display names", async () => {
    const resources = new AgentAvatarResources();
    const load = vi.fn(async () => ({ blob: png() }));
    resources.configure(load, capabilities);
    for (const item of [
      request(), request("b"), { ...request(), version: "other-version" },
      { ...request(), size: 64 as const }, { ...request(), expression: "waiting" as const },
    ]) resources.retain(item, vi.fn(), resources.getEpoch());
    await vi.waitFor(() => expect(load).toHaveBeenCalledTimes(5));
    resources.configure(load, capabilities);
    expect(createUrl).toHaveBeenCalledTimes(5);
    resources.reset();
  });

  it("bounds concurrency while queues and in-flight requests remain deduplicated", async () => {
    const resources = new AgentAvatarResources({ ...limits, concurrency: 1 });
    const first = deferred<{ blob: Blob }>();
    const load = vi.fn().mockImplementationOnce(() => first.promise)
      .mockResolvedValue({ blob: png() });
    resources.configure(load, capabilities);
    resources.retain(request("a"), vi.fn(), resources.getEpoch());
    resources.retain(request("b"), vi.fn(), resources.getEpoch());
    resources.retain(request("b"), vi.fn(), resources.getEpoch());
    await vi.waitFor(() => expect(load).toHaveBeenCalledOnce());
    first.resolve({ blob: png() });
    await vi.waitFor(() => expect(load).toHaveBeenCalledTimes(2));
    resources.reset();
  });

  it.each([32, 128])("loads a %s-Agent roster without blocking or duplicate navigation requests", async (count) => {
    const resources = new AgentAvatarResources();
    let active = 0;
    let peak = 0;
    const load = vi.fn(async () => {
      peak = Math.max(peak, ++active);
      await Promise.resolve();
      active -= 1;
      return { blob: png() };
    });
    resources.configure(load, capabilities);
    const releases: Array<() => void> = [];
    for (let index = 0; index < count; index += 1) {
      for (let surface = 0; surface < 2; surface += 1) {
        releases.push(resources.retain(request(String(index)), vi.fn(), resources.getEpoch()));
      }
    }
    expect(load).not.toHaveBeenCalled();
    await vi.waitFor(() => expect(createUrl).toHaveBeenCalledTimes(count));
    expect(load).toHaveBeenCalledTimes(count);
    expect(peak).toBeLessThanOrEqual(4);
    releases.forEach((release) => release());
    expect(revokeUrl).toHaveBeenCalledTimes(count);
    resources.reset();
  });

  it("cancels abandoned work before starting it", async () => {
    const resources = new AgentAvatarResources();
    const load = vi.fn(async () => ({ blob: png() }));
    resources.configure(load, capabilities);
    const release = resources.retain(request(), vi.fn(), resources.getEpoch());
    release();
    await Promise.resolve();
    expect(load).not.toHaveBeenCalled();
    resources.reset();
  });

  it("switches auth/connection epoch immediately and fences ignored aborts and late completions", async () => {
    const resources = new AgentAvatarResources();
    const old = deferred<{ blob: Blob }>();
    let oldSignal!: AbortSignal;
    const oldLoader = vi.fn((_id, options) => { oldSignal = options.signal; return old.promise; });
    resources.configure(oldLoader, capabilities);
    const oldEpoch = resources.getEpoch();
    resources.retain(request(), vi.fn(), oldEpoch);
    await vi.waitFor(() => expect(oldLoader).toHaveBeenCalledOnce());
    resources.reset();
    expect(oldSignal.aborted).toBe(true);
    expect(resources.peek(request(), oldEpoch)).toBeUndefined();
    const newLoader = vi.fn(async () => ({ blob: png() }));
    resources.configure(newLoader, capabilities);
    resources.retain(request(), vi.fn(), resources.getEpoch());
    await vi.waitFor(() => expect(createUrl).toHaveBeenCalledOnce());
    old.resolve({ blob: png() });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(createUrl).toHaveBeenCalledOnce();
    resources.reset();
    expect(revokeUrl).toHaveBeenCalledExactlyOnceWith("blob:avatar-1");
    resources.retain(request(), vi.fn(), oldEpoch);
    expect(newLoader).toHaveBeenCalledOnce();
  });

  it.each([new Error("offline"), { status: 401 }, { status: 404 }, { status: 503 }])(
    "does not turn a request failure into permanent API disablement: %o",
    async (error) => {
      const resources = new AgentAvatarResources();
      const load = vi.fn().mockRejectedValueOnce(error).mockResolvedValue({ blob: png() });
      resources.configure(load, capabilities);
      const listener = vi.fn();
      resources.retain(request(), listener, resources.getEpoch());
      await vi.waitFor(() => expect(listener).toHaveBeenCalledOnce());
      expect(resources.peek(request(), resources.getEpoch())).toBeUndefined();
      resources.retain(request(), vi.fn(), resources.getEpoch());
      await vi.waitFor(() => expect(createUrl).toHaveBeenCalledOnce());
      expect(load).toHaveBeenCalledTimes(2);
      resources.reset();
    },
  );

  it("evicts idle least-recently-used Blobs by entry budget", async () => {
    const resources = new AgentAvatarResources({ ...limits, entries: 1 });
    const load = vi.fn(async () => ({ blob: png() }));
    resources.configure(load, capabilities);
    const a = resources.retain(request("a"), vi.fn(), resources.getEpoch());
    await vi.waitFor(() => expect(createUrl).toHaveBeenCalledTimes(1));
    // A live URL must not be revoked just to admit another identity.
    resources.retain(request("b"), vi.fn(), resources.getEpoch());
    expect(load).toHaveBeenCalledTimes(1);
    a();
    const b = resources.retain(request("b"), vi.fn(), resources.getEpoch());
    await vi.waitFor(() => expect(createUrl).toHaveBeenCalledTimes(2));
    b();
    resources.retain(request("a"), vi.fn(), resources.getEpoch());
    await vi.waitFor(() => expect(load).toHaveBeenCalledTimes(3));
    resources.reset();
  });

  it("bounds retained bytes independently of entry count", async () => {
    const resources = new AgentAvatarResources({ ...limits, bytes: 6 });
    const load = vi.fn(async () => ({ blob: png(4) }));
    resources.configure(load, capabilities);
    const a = resources.retain(request("a"), vi.fn(), resources.getEpoch());
    await vi.waitFor(() => expect(createUrl).toHaveBeenCalledTimes(1));
    a();
    const b = resources.retain(request("b"), vi.fn(), resources.getEpoch());
    await vi.waitFor(() => expect(createUrl).toHaveBeenCalledTimes(2));
    b();
    resources.retain(request("a"), vi.fn(), resources.getEpoch());
    await vi.waitFor(() => expect(load).toHaveBeenCalledTimes(3));
    resources.reset();
  });

  it.each([new Blob(["not an image"], { type: "text/html" }), png(129), png(0)])(
    "rejects invalid content types, oversized images, and empty images", async (blob) => {
      const resources = new AgentAvatarResources(limits);
      const listener = vi.fn();
      resources.configure(async () => ({ blob }), capabilities);
      resources.retain(request(), listener, resources.getEpoch());
      await vi.waitFor(() => expect(listener).toHaveBeenCalledOnce());
      expect(createUrl).not.toHaveBeenCalled();
      resources.reset();
    },
  );
});
