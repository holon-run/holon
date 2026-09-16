import { afterEach, describe, expect, it } from "vitest";

import type {
  ConversationClientLike,
  ConversationStreamItem,
} from "@holon/conversation-sdk";

import {
  acquireConversationScope,
  CONVERSATION_SCOPE_KEEP_ALIVE,
  activeConversationScopeCount,
  conversationScopeKey,
  disposeAllConversationScopes,
  idleConversationScopeCount,
  peekConversationScope,
  releaseConversationScope,
  useConversationScopeStore,
} from "./conversation-scope-store";

class StreamQueue {
  #items: unknown[] = [];
  #waiters: Array<(result: { done: boolean; value?: unknown }) => void> = [];
  #closed = false;

  push(item: unknown): void {
    const waiter = this.#waiters.shift();
    if (waiter !== undefined) {
      waiter({ done: false, value: item });
    } else {
      this.#items.push(item);
    }
  }

  async *iterate(signal: AbortSignal) {
    while (true) {
      const item = this.#items.shift();
      if (item !== undefined) {
        yield item;
        continue;
      }
      if (this.#closed) return;
      await new Promise<void>((resolve, reject) => {
        const waiter = (result: { done: boolean; value?: unknown }) => {
          signal.removeEventListener("abort", onAbort);
          if (result.done) {
            resolve();
            return;
          }
          this.#items.push(result.value);
          resolve();
        };
        const onAbort = () => {
          const index = this.#waiters.indexOf(waiter);
          if (index !== -1) this.#waiters.splice(index, 1);
          reject(signal.reason ?? new Error("aborted"));
        };
        signal.addEventListener("abort", onAbort, { once: true });
        this.#waiters.push(waiter);
      });
    }
  }
}

function summarySnapshot(throughSeq = 10) {
  return {
    schema_version: 1,
    query_version: 1,
    runtime_id: "runtime-a",
    event_log_epoch: "epoch-a",
    visibility_scope_id: "scope-a",
    snapshot_through_seq: throughSeq,
    event_head_seq: throughSeq,
    oldest_retained_seq: 1,
    snapshot_cursor: `checkpoint-${throughSeq}`,
    turns: [
      {
        turn_id: `turn-${throughSeq}`,
        key: { turn_index: throughSeq, turn_id: `turn-${throughSeq}` },
        revision: 1,
        presentation_class: "operator",
        execution: { kind: "active" },
        result: { kind: "pending" },
        settled: false,
        attention: null,
        detail_coverage: { kind: "complete" },
        brief_ids: [],
      },
    ],
    active_turns: [],
    pending_inputs: [],
    next_before_cursor: null,
    has_more: false,
  };
}

function fakeClient() {
  const hub = new StreamQueue();
  const calls = { capability: 0, summary: 0, stream: 0, open: 0 };
  const client: ConversationClientLike = {
    baseUrl: "http://fake.local/api",
    async requireCapability() {
      calls.capability += 1;
      return { ok: true };
    },
    async summary() {
      calls.summary += 1;
      return { summary: summarySnapshot(), etag: null } as never;
    },
    async activities() {
      throw new Error("not used");
    },
    async brief() {
      throw new Error("not used");
    },
    async *stream(_agent, options): AsyncGenerator<ConversationStreamItem> {
      calls.stream += 1;
      calls.open += 1;
      try { yield* hub.iterate(options?.signal ?? new AbortController().signal) as AsyncGenerator<ConversationStreamItem>; }
      finally { calls.open -= 1; }
    },
  };
  return { client, hub, calls };
}

async function waitFor(predicate: () => boolean, timeoutMs = 2_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
  expect(predicate()).toBe(true);
}

const sleep = async () => {};

afterEach(() => {
  disposeAllConversationScopes();
});

describe("conversation scope store", () => {
  it("shares one controller per scope key and retains its data after last release", async () => {
    const { client, calls } = fakeClient();
    const key = conversationScopeKey("local", "web");
    const first = acquireConversationScope({
      key,
      agentId: "web",
      baseUrl: "http://fake.local/api",
      clientFactory: () => client,
      controllerOptions: { sleep, random: () => 0.5 },
    });
    const second = acquireConversationScope({
      key,
      agentId: "web",
      baseUrl: "http://fake.local/api",
      clientFactory: () => client,
      controllerOptions: { sleep, random: () => 0.5 },
    });
    expect(second.controller).toBe(first.controller);
    await waitFor(() => first.controller.status.kind === "ready");
    expect(calls.capability).toBe(1);
    expect(calls.summary).toBe(1);
    expect(peekConversationScope(key)).not.toBeNull();

    releaseConversationScope(key);
    // Last release retains the data but closes the idle stream.
    expect(peekConversationScope(key)).not.toBeNull();
    releaseConversationScope(key);
    expect(peekConversationScope(key)).not.toBeNull();
    expect(idleConversationScopeCount()).toBe(1);
    await waitFor(() => calls.open === 0);

    const third = acquireConversationScope({
      key,
      agentId: "web",
      baseUrl: "http://fake.local/api",
      clientFactory: () => client,
      controllerOptions: { sleep, random: () => 0.5 },
    });
    expect(third.controller).toBe(first.controller);
    expect(calls.capability).toBe(1);
    expect(calls.summary).toBe(1);
    expect(idleConversationScopeCount()).toBe(0);
    await waitFor(() => calls.stream === 2);
    expect(calls.open).toBe(1);
  });

  it("publishes view snapshots into the mirror store", async () => {
    const { client } = fakeClient();
    const key = conversationScopeKey("local", "web");
    acquireConversationScope({
      key,
      agentId: "web",
      baseUrl: "http://fake.local/api",
      clientFactory: () => client,
      controllerOptions: { sleep, random: () => 0.5 },
    });
    await waitFor(() => firstMirrorTurn(key) === "turn-10");
    const mirror = useConversationScopeStore.getState().scopes[key];
    expect(mirror?.status.kind).toBe("ready");
    expect(mirror?.view?.turns).toHaveLength(1);
    expect(mirror?.version).toBeGreaterThan(0);
    releaseConversationScope(key);
  });

  it("isolates scopes per agent", () => {
    const { client } = fakeClient();
    const keyA = conversationScopeKey("local", "agent-a");
    const keyB = conversationScopeKey("local", "agent-b");
    const a = acquireConversationScope({
      key: keyA,
      agentId: "agent-a",
      baseUrl: "http://fake.local/api",
      clientFactory: () => client,
      controllerOptions: { sleep, random: () => 0.5 },
    });
    const b = acquireConversationScope({
      key: keyB,
      agentId: "agent-b",
      baseUrl: "http://fake.local/api",
      clientFactory: () => client,
      controllerOptions: { sleep, random: () => 0.5 },
    });
    expect(a.controller).not.toBe(b.controller);
    expect(activeConversationScopeCount()).toBe(2);
    releaseConversationScope(keyA);
    releaseConversationScope(keyB);
    expect(idleConversationScopeCount()).toBe(2);
  });

  it("evicts idle scopes in LRU order beyond the keep-alive budget", async () => {
    const scopes = ["a", "b", "c", "d"].map((agent) => {
      const fake = fakeClient();
      return {
        agent,
        fake,
        key: conversationScopeKey("local", agent),
        handle: acquireConversationScope({
          key: conversationScopeKey("local", agent),
          agentId: agent,
          baseUrl: "http://fake.local/api",
          clientFactory: () => fake.client,
          controllerOptions: { sleep, random: () => 0.5 },
        }),
      };
    });
    await waitFor(() => scopes.every(({ handle }) => handle.controller.status.kind === "ready"));
    for (const { key } of scopes) {
      releaseConversationScope(key);
    }
    expect(idleConversationScopeCount()).toBe(CONVERSATION_SCOPE_KEEP_ALIVE);
    // "a" was released first and is the LRU victim.
    expect(peekConversationScope(scopes[0].key)).toBeNull();
    expect(useConversationScopeStore.getState().scopes[scopes[0].key]).toBeUndefined();
    for (const scope of scopes.slice(1)) {
      expect(peekConversationScope(scope.key)).not.toBeNull();
    }

    // Reacquiring a kept-alive scope reuses the controller without refetching.
    const reused = acquireConversationScope({
      key: scopes[1].key,
      agentId: scopes[1].agent,
      baseUrl: "http://fake.local/api",
      clientFactory: () => scopes[1].fake.client,
      controllerOptions: { sleep, random: () => 0.5 },
    });
    expect(reused.controller).toBe(scopes[1].handle.controller);
    expect(scopes[1].fake.calls.summary).toBe(1);
    expect(idleConversationScopeCount()).toBe(2);
  });

  it("restarts a kept-alive controller that died while idle", async () => {
    const { client, calls } = fakeClient();
    let capabilityAttempts = 0;
    (client as { requireCapability: () => Promise<unknown> }).requireCapability =
      async () => {
        capabilityAttempts += 1;
        if (capabilityAttempts === 1) {
          throw new (class CapabilityError extends Error {})();
        }
        return { ok: true };
      };
    const key = conversationScopeKey("local", "web");
    const first = acquireConversationScope({
      key,
      agentId: "web",
      baseUrl: "http://fake.local/api",
      clientFactory: () => client,
      controllerOptions: { sleep, random: () => 0.5 },
    });
    await waitFor(() => first.controller.status.kind === "terminal_error");
    releaseConversationScope(key);
    const second = acquireConversationScope({
      key,
      agentId: "web",
      baseUrl: "http://fake.local/api",
      clientFactory: () => client,
      controllerOptions: { sleep, random: () => 0.5 },
    });
    expect(second.controller).toBe(first.controller);
    await waitFor(() => second.controller.status.kind === "ready");
    expect(capabilityAttempts).toBeGreaterThanOrEqual(2);
  });
});

function firstMirrorTurn(key: string): string | null {
  const turn = useConversationScopeStore.getState().scopes[key]?.view?.turns[0];
  return turn?.turn_id ?? null;
}
