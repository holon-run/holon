import { describe, expect, it, vi } from "vitest";
import type { ConversationClientLike } from "@holon/conversation-sdk";
import { conversationNetworkGate } from "./conversation-network-gate";

function deferred<T = never>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
function client(): ConversationClientLike {
  return {
    baseUrl: "/api",
    requireCapability: vi.fn(async () => ({})),
    summary: vi.fn(async () => ({} as never)),
    activities: vi.fn(async () => ({} as never)),
    brief: vi.fn(async () => ({} as never)),
    locateSource: vi.fn(async () => ({} as never)),
    async *stream() {},
  };
}

describe("read-only conversation network lifecycle", () => {
  it("rejects every new operation before calling the client during preview", () => {
    const source = client();
    const gate = conversationNetworkGate(source);
    const operations = [
      () => gate.client.requireCapability(),
      () => gate.client.summary("a"),
      () => gate.client.activities("a", "t"),
      () => gate.client.brief("a", "b"),
      () => gate.client.locateSource!("a", "source"),
    ];
    for (const operation of operations) expect(operation).toThrow();
    expect(source.requireCapability).not.toHaveBeenCalled();
    expect(source.summary).not.toHaveBeenCalled();
    expect(source.activities).not.toHaveBeenCalled();
    expect(source.brief).not.toHaveBeenCalled();
    expect(source.locateSource).not.toHaveBeenCalled();
  });

  it.each(["requireCapability", "summary", "activities", "brief", "locateSource"] as const)(
    "parks interrupted %s and refetches only after resume", async (operation) => {
      const pending = deferred();
      const source = client();
      const invoke = vi.fn().mockReturnValueOnce(pending.promise).mockResolvedValue({ fresh: true });
      source[operation] = invoke;
      const gate = conversationNetworkGate(source);
      gate.resume();
      const result = operation === "requireCapability" ? gate.client.requireCapability()
        : operation === "summary" ? gate.client.summary("a")
          : operation === "activities" ? gate.client.activities("a", "t")
            : operation === "brief" ? gate.client.brief("a", "b")
              : gate.client.locateSource!("a", "ref");
      let settled = false;
      void result.then(() => { settled = true; });
      await Promise.resolve();
      gate.pause();
      pending.resolve({} as never);
      await Promise.resolve();
      expect(settled).toBe(false);
      expect(invoke).toHaveBeenCalledTimes(1);
      gate.resume();
      await expect(result).resolves.toEqual({ fresh: true });
      expect(invoke).toHaveBeenCalledTimes(2);
    },
  );

  it("rejects parked requests on disposal or caller cancellation", async () => {
    for (const reason of ["dispose", "caller"] as const) {
      const pending = deferred();
      const source = client();
      source.brief = () => pending.promise;
      const gate = conversationNetworkGate(source);
      const caller = new AbortController();
      gate.resume();
      const result = gate.client.brief("a", "b", caller.signal);
      const rejected = expect(result).rejects.toMatchObject({ name: "AbortError" });
      await Promise.resolve();
      gate.pause();
      await Promise.resolve();
      if (reason === "dispose") gate.dispose();
      else caller.abort();
      await rejected;
    }
  });

  it("never delivers a queued old stream item after a resumed generation", async () => {
    const pending = deferred();
    const source = client();
    source.stream = async function* () { yield await pending.promise; };
    const gate = conversationNetworkGate(source);
    gate.resume();
    const iterator = gate.client.stream("a");
    const item = iterator.next();
    const rejected = expect(item).rejects.toMatchObject({ name: "AbortError" });
    gate.pause();
    gate.resume();
    pending.resolve({} as never);
    await rejected;
  });
});
