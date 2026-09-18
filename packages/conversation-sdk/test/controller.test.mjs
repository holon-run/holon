import assert from "node:assert/strict";
import test from "node:test";

import {
  ConversationCapabilityError,
  ConversationController,
  ConversationProtocolError,
  ConversationResetError,
} from "../dist/index.js";
import { activity, batch, detail, identity, summary, turn } from "./helpers.mjs";

function briefRecord(id) {
  return {
    id,
    agent_id: identity.agent_id,
    workspace_id: "ws-a",
    kind: "result",
    created_at: "2026-01-01T00:00:00Z",
    content_source: { kind: "inline" },
    text: `brief ${id}`,
    attachments: null,
    related_message_id: null,
    related_task_id: null,
  };
}

class StreamQueue {
  #items = [];
  #waiters = [];
  #closed = false;

  push(item) {
    if (this.#waiters.length > 0) {
      this.#waiters.shift()({ done: false, value: item });
    } else {
      this.#items.push(item);
    }
  }

  close() {
    this.#closed = true;
    for (const waiter of this.#waiters.splice(0)) {
      waiter({ done: true, value: undefined });
    }
  }

  async *iterate(signal) {
    while (true) {
      if (this.#items.length > 0) {
        yield this.#items.shift();
        continue;
      }
      if (this.#closed) {
        return;
      }
      const next = await new Promise((resolve, reject) => {
        const waiter = (result) => {
          signal.removeEventListener("abort", onAbort);
          resolve(result);
        };
        const onAbort = () => {
          const index = this.#waiters.indexOf(waiter);
          if (index !== -1) {
            this.#waiters.splice(index, 1);
          }
          reject(signal.reason ?? new Error("aborted"));
        };
        signal.addEventListener("abort", onAbort, { once: true });
        this.#waiters.push(waiter);
      });
      if (next.done) {
        return;
      }
      yield next.value;
    }
  }
}

function fakeClient(overrides = {}) {
  const calls = {
    capability: 0,
    summary: [],
    activities: [],
    brief: [],
    stream: [],
    openStreams: 0,
    maxOpenStreams: 0,
  };
  const client = {
    baseUrl: "http://fake.local",
    async requireCapability() {
      calls.capability += 1;
      if (overrides.capabilityError) {
        throw overrides.capabilityError;
      }
      return { ok: true };
    },
    async summary(agentId, options = {}) {
      calls.summary.push({ agentId, ...options });
      const handler = overrides.summary;
      let result;
      if (handler) {
        result = handler(options, calls.summary.length);
      } else {
        result = summary();
      }
      return normalizeSummaryResult(result);
    },
    async activities(agentId, turnId, options = {}) {
      calls.activities.push({ agentId, turnId, ...options });
      const handler = overrides.activities;
      if (handler) {
        return handler(turnId, options, calls.activities.length);
      }
      return detail({ turn_id: turnId });
    },
    async brief(agentId, briefId, signal) {
      calls.brief.push({ agentId, briefId });
      const handler = overrides.brief;
      if (handler) {
        return handler(briefId, calls.brief.length);
      }
      return briefRecord(briefId);
    },
    async *stream(agentId, options = {}) {
      calls.stream.push({ agentId, ...options });
      calls.openStreams += 1;
      calls.maxOpenStreams = Math.max(calls.maxOpenStreams, calls.openStreams);
      try {
        const handler = overrides.stream;
        if (handler) {
          yield* handler(options, calls.stream.length, calls);
          return;
        }
        yield* overrides.hub.iterate(options.signal);
      } finally {
        calls.openStreams -= 1;
      }
    },
    calls,
  };
  return client;
}

// Accept either a raw summary snapshot (legacy handler style) or a
// { summary, etag } result, including 304-style { summary: null, etag }.
async function normalizeSummaryResult(result) {
  const value = await result;
  if (
    result &&
    value &&
    typeof result === "object" &&
    typeof value === "object" &&
    "summary" in value &&
    !("turns" in value)
  ) {
    return value;
  }
  return { summary: value ?? null, etag: null };
}

function immediateSleep(log = []) {
  return async (ms) => {
    log.push(ms);
  };
}

function deferredSleep() {
  const log = [];
  const pending = [];
  const sleep = async (ms, signal) => {
    log.push(ms);
    await new Promise((resolve, reject) => {
      const entry = { resolve, reject };
      pending.push(entry);
      signal.addEventListener(
        "abort",
        () => reject(signal.reason ?? new Error("aborted")),
        { once: true },
      );
    });
  };
  return {
    sleep,
    log,
    pending,
    async flush() {
      const entry = pending.shift();
      entry?.resolve();
    },
  };
}

const fixedRandom = (value) => () => value;

async function waitFor(predicate, timeoutMs = 2_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
  assert.ok(predicate(), "waitFor timed out");
}

test("bootstraps to ready and applies stream batches", async () => {
  const hub = new StreamQueue();
  const client = fakeClient({ hub });
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    remoteId: identity.remote_id,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  const changes = [];
  controller.subscribe(() => changes.push(controller.status.kind));
  controller.start();
  await waitFor(() => controller.status.kind === "ready");
  assert.equal(client.calls.capability, 1);
  assert.equal(client.calls.summary.length, 1);
  assert.equal(client.calls.summary[0].limit, 30);
  assert.equal(client.calls.stream.length, 1);
  assert.equal(client.calls.stream[0].after, "checkpoint-10");
  assert.deepEqual(
    controller.view().turns.map((entry) => entry.turn_id),
    ["turn-10"],
  );

  hub.push({
    type: "batch",
    batch: batch({
      from: 10,
      through: 11,
      mutations: [
        {
          type: "turn_summary_upsert",
          event_log_epoch: "epoch-a",
          visibility_scope_id: "scope-a",
          turn: turn("turn-11", 11, 1, {
            execution: { kind: "terminal", outcome: "completed" },
            result: { kind: "available" },
            settled: true,
            brief_ids: ["brief-11"],
          }),
        },
      ],
    }),
  });
  await waitFor(() =>
    controller.view().turns.some((entry) => entry.turn_id === "turn-11"),
  );
  assert.equal(controller.view().through_seq, 11);
  assert.ok(changes.includes("ready"));
  controller.dispose();
});

test("network failure reconnects with bounded backoff and resumes checkpoint", async () => {
  const delays = [];
  const hub = new StreamQueue();
  const client = fakeClient({ hub });
  let streamCalls = 0;
  const statusKinds = [];
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    retry: { initialDelayMs: 500, maxDelayMs: 30_000, jitterRatio: 0 },
    sleep: async (ms) => {
      delays.push(ms);
    },
    random: fixedRandom(0.5),
    onEvent: (event) => {
      if (event.type === "stream_error") {
        streamCalls = event.attempt;
      }
      if (event.type === "status") {
        statusKinds.push(event.status.kind);
      }
    },
  });
  client.overrides = {};
  // First stream attempt fails with a network-class error.
  const originalStream = client.stream.bind(client);
  let attempt = 0;
  client.stream = async function* (agentId, options = {}) {
    attempt += 1;
    if (attempt === 1) {
      client.calls.stream.push({ agentId, ...options });
      throw new TypeError("fetch failed");
    }
    yield* originalStream(agentId, options);
  };
  controller.start();
  await waitFor(() =>
    statusKinds.includes("reconnecting") && controller.status.kind === "ready",
  );
  assert.equal(streamCalls, 1);
  assert.deepEqual(delays, [500]);
  const second = client.calls.stream.at(-1);
  assert.equal(second.after, "checkpoint-10");
  controller.dispose();
});

test("reset-required attach recovers via a fresh snapshot instead of retrying the stale checkpoint", async () => {
  const hub = new StreamQueue();
  const client = fakeClient({ hub });
  const statusKinds = [];
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    retry: { initialDelayMs: 100, jitterRatio: 0 },
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
    onEvent: (event) => {
      if (event.type === "status") {
        statusKinds.push(event.status.kind);
      }
    },
  });
  const originalStream = client.stream.bind(client);
  let streamAttempt = 0;
  client.stream = async function* (agentId, options = {}) {
    streamAttempt += 1;
    if (streamAttempt === 1) {
      throw new ConversationResetError("replay_limit_exceeded");
    }
    yield* originalStream(agentId, options);
  };
  controller.start();
  await waitFor(() => controller.status.kind === "ready" && streamAttempt >= 2);
  // The reset must trigger a fresh bootstrap (second summary) and the retry
  // must attach from the fresh snapshot checkpoint, not loop on the stale one.
  assert.equal(client.calls.summary.length, 2);
  assert.equal(client.calls.stream.length, 1);
  assert.equal(client.calls.stream[0].after, "checkpoint-10");
  assert.ok(statusKinds.includes("reconnecting"));
  assert.equal(controller.status.kind, "ready");
  controller.dispose();
});

test("exhausted retries surface a recoverable error and retry() restarts", async () => {
  const client = fakeClient();
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    retry: { maxAttempts: 1, initialDelayMs: 100, jitterRatio: 0 },
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  client.stream = async function* () {
    throw new TypeError("fetch failed");
  };
  controller.start();
  await waitFor(() => controller.status.kind === "recoverable_error");
  const summariesBefore = client.calls.summary.length;

  controller.retry();
  await waitFor(() => controller.status.kind === "recoverable_error");
  assert.ok(client.calls.summary.length >= summariesBefore);
  controller.dispose();
});

test("backoff delay grows geometrically and is jitter-bounded", async () => {
  const client = fakeClient();
  client.stream = async function* () {
    throw new TypeError("fetch failed");
  };
  const delays = [];
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    retry: { maxAttempts: 4, initialDelayMs: 100, maxDelayMs: 1_000, jitterRatio: 0.5 },
    sleep: async (ms) => {
      delays.push(ms);
    },
    random: fixedRandom(1),
  });
  controller.start();
  await waitFor(() => controller.status.kind === "recoverable_error");
  assert.deepEqual(delays, [150, 300, 600, 1000]);
  controller.dispose();
});

test("reset_required triggers a serial re-bootstrap without concurrent streams", async () => {
  const hub = new StreamQueue();
  const client = fakeClient({ hub });
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  controller.start();
  await waitFor(() => controller.status.kind === "ready");
  hub.push({
    type: "reset_required",
    reset: {
      type: "reset_required",
      reason: "retention_expired",
      oldest_retained_seq: 5,
      event_head_seq: 50,
      hint: "expired",
    },
  });
  await waitFor(() => controller.status.kind === "ready" && client.calls.summary.length === 2);
  assert.equal(client.calls.maxOpenStreams, 1);
  assert.equal(client.calls.stream.length, 2);
  assert.equal(client.calls.stream[1].after, "checkpoint-10");
  controller.dispose();
});

test("stale stream batch self-heals through reset and re-bootstrap", async () => {
  const hub = new StreamQueue();
  const client = fakeClient({ hub });
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  controller.start();
  await waitFor(() => controller.status.kind === "ready");
  hub.push({
    type: "batch",
    batch: batch({ from: 999, through: 1000 }),
  });
  await waitFor(() => client.calls.summary.length === 2);
  assert.equal(controller.view().reset_reason, null);
  await waitFor(() => controller.status.kind === "ready");
  controller.dispose();
});

test("capability failure lands in unsupported without snapshot or stream", async () => {
  const client = fakeClient({
    capabilityError: new ConversationCapabilityError(
      "agents.conversation-read.v1",
    ),
  });
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  controller.start();
  await waitFor(() => controller.status.kind === "unsupported");
  assert.equal(client.calls.summary.length, 0);
  assert.equal(client.calls.stream.length, 0);
  controller.dispose();
});

test("dispose cancels pending backoff and blocks further work", async () => {
  const client = fakeClient();
  client.stream = async function* () {
    throw new TypeError("fetch failed");
  };
  const sleepState = deferredSleep();
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    retry: { maxAttempts: 5 },
    sleep: sleepState.sleep,
    random: fixedRandom(0.5),
  });
  const changes = [];
  const unsubscribe = controller.subscribe(() => changes.push(1));
  controller.start();
  await waitFor(() => controller.status.kind === "reconnecting");
  const streamsBefore = client.calls.stream.length;
  controller.dispose();
  await sleepState.flush().catch(() => {});
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(client.calls.stream.length, streamsBefore);
  assert.throws(() => controller.start(), ConversationProtocolError);
  const briefState = await controller.loadBrief("brief-1");
  assert.equal(briefState.kind, "error");
  assert.equal(briefState.retryable, false);
  unsubscribe();
});

test("history pagination is single-flight and completes at the last page", async () => {
  let releaseFirst;
  const gate = new Promise((resolve) => {
    releaseFirst = resolve;
  });
  const olderPage = summary({
    snapshot_through_seq: 10,
    event_head_seq: 10,
    turns: [turn("turn-9", 9), turn("turn-8", 8)],
    next_before_cursor: null,
    has_more: false,
  });
  const client = fakeClient({
    summary: (options, index) => {
      if (index === 2) {
        return gate.then(() => olderPage);
      }
      return summary();
    },
  });
  const hub = new StreamQueue();
  const originalStream = client.stream.bind(client);
  client.stream = async function* (agentId, options = {}) {
    client.calls.stream.push({ agentId, ...options });
    yield* hub.iterate(options.signal);
  };
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  controller.start();
  await waitFor(() => controller.status.kind === "ready");

  const first = controller.loadOlderHistory();
  const second = controller.loadOlderHistory();
  assert.equal(controller.historyState().kind, "loading");
  releaseFirst();
  const [firstState, secondState] = await Promise.all([first, second]);
  assert.equal(firstState.kind, "complete");
  // Single-flight: the concurrent caller observes the loading snapshot.
  assert.equal(secondState.kind, "loading");
  assert.equal(controller.historyState().kind, "complete");
  assert.equal(client.calls.summary.length, 2);
  assert.equal(client.calls.summary[1].before, "history-before-10");
  const view = controller.view();
  assert.deepEqual(
    view.turns.map((entry) => entry.turn_id),
    ["turn-8", "turn-9", "turn-10"],
  );
  assert.equal(view.has_more, false);
  const third = await controller.loadOlderHistory();
  assert.equal(third.kind, "complete");
  assert.equal(client.calls.summary.length, 2);
  controller.dispose();
});

test("history load before bootstrap reports a retryable error", async () => {
  const client = fakeClient();
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  const state = await controller.loadOlderHistory();
  assert.equal(state.kind, "error");
  assert.equal(state.retryable, true);
  controller.dispose();
});

test("detail loads first page and pages older activities by cursor", async () => {
  const hub = new StreamQueue();
  const olderDetail = detail({
    turn: turn("turn-10", 10),
    detail_revision: 1,
    activities: [activity("activity-9", 9)],
    next_before_cursor: null,
    has_more: false,
  });
  const client = fakeClient({
    hub,
    activities: (turnId, options, index) => {
      if (index === 2) {
        assert.equal(options.before, "detail-before-10");
        return olderDetail;
      }
      return detail({
        turn: turn(turnId, 10),
        next_before_cursor: "detail-before-10",
        has_more: true,
      });
    },
  });
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  controller.start();
  await waitFor(() => controller.status.kind === "ready");
  const first = await controller.loadDetail("turn-10");
  assert.equal(first.kind, "idle");
  const detailView = controller.view().details[0];
  assert.equal(detailView.has_more, true);
  assert.equal(detailView.next_before_cursor, "detail-before-10");

  const concurrent = await Promise.all([
    controller.loadOlderActivities("turn-10"),
    controller.loadOlderActivities("turn-10"),
  ]);
  assert.equal(concurrent[0].kind, "idle");
  // Single-flight: the concurrent caller observes the loading snapshot.
  assert.equal(concurrent[1].kind, "loading");
  assert.equal(client.calls.activities.length, 2);
  const merged = controller.view().details[0];
  assert.equal(merged.has_more, false);
  assert.deepEqual(
    merged.activities.map((entry) => entry.id),
    ["activity-9", "activity-10"],
  );
  controller.dispose();
});

test("brief loads are deduped, cached, and bounded", async () => {
  let releaseBrief;
  const gate = new Promise((resolve) => {
    releaseBrief = resolve;
  });
  const client = fakeClient({
    brief: (briefId) =>
      gate.then(() => briefRecord(briefId)),
  });
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    maxBriefCache: 2,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  const first = controller.loadBrief("brief-1");
  const second = controller.loadBrief("brief-1");
  assert.equal(controller.briefState("brief-1").kind, "loading");
  releaseBrief();
  const [firstState, secondState] = await Promise.all([first, second]);
  assert.equal(firstState.kind, "ready");
  // Single-flight: the concurrent caller observes the loading snapshot.
  assert.equal(secondState.kind, "loading");
  assert.equal(controller.briefState("brief-1").kind, "ready");
  assert.equal(client.calls.brief.length, 1);

  const cached = await controller.loadBrief("brief-1");
  assert.equal(cached.kind, "ready");
  assert.equal(client.calls.brief.length, 1);

  await controller.loadBrief("brief-2");
  await controller.loadBrief("brief-3");
  assert.equal(client.calls.brief.length, 3);
  assert.equal(controller.briefState("brief-1"), null);
  assert.notEqual(controller.briefState("brief-3"), null);
  controller.dispose();
});

test("brief cache hits skip the network and fetched briefs are persisted", async () => {
  const client = fakeClient();
  const store = new Map();
  const briefCache = {
    async get(briefId) {
      return store.get(briefId) ?? null;
    },
    async put(briefId, brief) {
      store.set(briefId, brief);
    },
  };
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    briefCache,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  const fetched = await controller.loadBrief("brief-1");
  assert.equal(fetched.kind, "ready");
  assert.equal(client.calls.brief.length, 1);
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(store.has("brief-1"));
  controller.dispose();

  // A fresh controller (e.g. after a page reload) serves from the cache.
  const second = new ConversationController({
    client,
    agentId: identity.agent_id,
    briefCache,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  const cached = await second.loadBrief("brief-1");
  assert.equal(cached.kind, "ready");
  assert.equal(cached.brief.text, "brief brief-1");
  assert.equal(client.calls.brief.length, 1);
  second.dispose();
});

test("brief cache entries bound to another agent or id are ignored", async () => {
  const client = fakeClient();
  const foreign = { ...briefRecord("brief-1"), agent_id: "other-agent" };
  const briefCache = {
    async get(briefId) {
      return briefId === "brief-1" ? foreign : null;
    },
    async put() {},
  };
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    briefCache,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  const state = await controller.loadBrief("brief-1");
  assert.equal(state.kind, "ready");
  assert.equal(state.brief.agent_id, identity.agent_id);
  assert.equal(client.calls.brief.length, 1);
  controller.dispose();
});

test("snapshot cache hydrates instantly and a 304 revalidation skips refetch", async () => {
  const hub = new StreamQueue();
  const client = fakeClient({
    summary: (options) => {
      // The only summary call is the conditional revalidation.
      assert.equal(options.ifNoneMatch, "etag-10");
      return { summary: null, etag: "etag-10" };
    },
    hub,
  });
  let stored = { etag: "etag-10", summary: summary() };
  const snapshotCache = {
    async load() {
      return stored;
    },
    async store(entry) {
      stored = entry;
    },
  };
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    snapshotCache,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  controller.start();
  // The cached snapshot renders before the network confirms it.
  await waitFor(() => (controller.view().turns ?? []).length === 1);
  assert.equal(controller.view().checkpoint, "checkpoint-10");
  await waitFor(() => controller.status.kind === "ready");
  assert.equal(client.calls.summary.length, 1);
  assert.equal(client.calls.summary[0].ifNoneMatch, "etag-10");
  assert.equal(client.calls.stream.length, 1);
  assert.equal(client.calls.stream[0].after, "checkpoint-10");
  controller.dispose();
});

test("snapshot cache revalidates with a fresh snapshot when content changed", async () => {
  const hub = new StreamQueue();
  const fresh = summary({
    snapshot_through_seq: 12,
    event_head_seq: 12,
    snapshot_cursor: "checkpoint-12",
    turns: [turn("turn-12", 12)],
  });
  const client = fakeClient({
    summary: () => ({ summary: fresh, etag: "etag-12" }),
    hub,
  });
  let stored = { etag: "etag-10", summary: summary() };
  const snapshotCache = {
    async load() {
      return stored;
    },
    async store(entry) {
      stored = entry;
    },
  };
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    snapshotCache,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  controller.start();
  await waitFor(() => controller.status.kind === "ready");
  assert.equal(controller.view().checkpoint, "checkpoint-12");
  assert.equal(client.calls.summary.length, 1);
  assert.equal(client.calls.summary[0].ifNoneMatch, "etag-10");
  assert.equal(client.calls.stream[0].after, "checkpoint-12");
  // The fresh snapshot replaces the cached entry for the next open.
  await waitFor(() => stored?.etag === "etag-12");
  controller.dispose();
});

test("cached snapshot paints before the capability handshake resolves", async () => {
  const hub = new StreamQueue();
  let releaseCapability;
  const capabilityGate = new Promise((resolve) => {
    releaseCapability = resolve;
  });
  const client = fakeClient({
    summary: () => ({ summary: null, etag: "etag-10" }),
    hub,
  });
  client.requireCapability = async () => {
    await capabilityGate;
    return { ok: true };
  };
  const snapshotCache = {
    async load() {
      return { etag: "etag-10", summary: summary() };
    },
    async store() {},
  };
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    snapshotCache,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  controller.start();
  // The cached conversation renders while the handshake is still pending.
  await waitFor(() => (controller.view().turns ?? []).length === 1);
  assert.equal(controller.status.kind, "loading");
  releaseCapability();
  await waitFor(() => controller.status.kind === "ready");
  assert.equal(controller.view().checkpoint, "checkpoint-10");
  assert.equal(client.calls.stream[0].after, "checkpoint-10");
  controller.dispose();
});

test("fresh bootstraps persist their snapshot for later opens", async () => {
  const hub = new StreamQueue();
  const client = fakeClient({ hub });
  let stored = null;
  const snapshotCache = {
    async load() {
      return stored;
    },
    async store(entry) {
      stored = entry;
    },
  };
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    snapshotCache,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  controller.start();
  await waitFor(() => controller.status.kind === "ready");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(stored?.etag, null);
  assert.equal(stored?.summary.snapshot_through_seq, 10);
  controller.dispose();
});

test("incompatible cached snapshots fall back to a plain network bootstrap", async () => {
  const hub = new StreamQueue();
  const client = fakeClient({ hub });
  const snapshotCache = {
    async load() {
      return { etag: "etag-9", summary: summary({ schema_version: 2 }) };
    },
    async store() {},
  };
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    snapshotCache,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  controller.start();
  await waitFor(() => controller.status.kind === "ready");
  assert.equal(client.calls.summary.length, 1);
  assert.equal(client.calls.summary[0].ifNoneMatch, undefined);
  controller.dispose();
});

test("brief cache read or write failures fall back to the network", async () => {
  const client = fakeClient();
  const briefCache = {
    async get() {
      throw new Error("storage unavailable");
    },
    async put() {
      throw new Error("storage unavailable");
    },
  };
  const controller = new ConversationController({
    client,
    agentId: identity.agent_id,
    briefCache,
    sleep: immediateSleep(),
    random: fixedRandom(0.5),
  });
  const state = await controller.loadBrief("brief-1");
  assert.equal(state.kind, "ready");
  assert.equal(client.calls.brief.length, 1);
  controller.dispose();
});

test("pause releases the stream and resumes from the retained checkpoint without another summary", async () => {
  const hub = new StreamQueue();
  const client = fakeClient({ hub });
  const controller = new ConversationController({ client, agentId: identity.agent_id });
  controller.start();
  await waitFor(() => client.calls.openStreams === 1);
  controller.pause();
  await waitFor(() => client.calls.openStreams === 0);
  assert.equal(controller.view().checkpoint, "checkpoint-10");
  assert.equal(controller.status.kind, "paused");
  controller.start();
  await waitFor(() => client.calls.stream.length === 2);
  assert.equal(client.calls.summary.length, 1);
  assert.equal(client.calls.stream[1].after, "checkpoint-10");
  controller.dispose();
});

test("streamed terminal state is persisted with its checkpoint and without the old ETag", async () => {
  const hub = new StreamQueue();
  const client = fakeClient({ hub });
  let stored;
  const controller = new ConversationController({ client, agentId: identity.agent_id,
    snapshotCache: { async load() { return null; }, async store(entry) { stored = entry; } },
  });
  controller.start();
  await waitFor(() => client.calls.openStreams === 1 && stored);
  const terminal = turn("turn-10", 10, 2, { execution: { kind: "terminal", outcome: "completed" }, result: { kind: "none", reason: { kind: "tool_only_wait" } }, settled: true });
  hub.push({ type: "batch", batch: batch({ from: 10, through: 11, mutations: [{ type: "turn_summary_upsert", event_log_epoch: "epoch-a", visibility_scope_id: "scope-a", turn: terminal }] }) });
  await waitFor(() => controller.view().through_seq === 11);
  controller.pause();
  await waitFor(() => stored.summary.snapshot_through_seq === 11);
  assert.equal(stored.summary.turns[0].execution.kind, "terminal");
  assert.equal(stored.summary.active_turns.length, 0);
  assert.equal(stored.summary.snapshot_cursor, "checkpoint-11");
  assert.equal(stored.etag, null);
  controller.dispose();
});

test("forbidden revalidation clears cached content and persistent stores", async () => {
  const { ConversationHttpError } = await import("../dist/index.js");
  const client = fakeClient({ summary() { throw new ConversationHttpError(403, { ok: false, error: "forbidden" }); } });
  let cleared = 0;
  const controller = new ConversationController({ client, agentId: identity.agent_id,
    snapshotCache: { async load() { return { summary: summary(), etag: "old" }; }, async store() {}, async clear() { cleared++; } },
    briefCache: { async get() {}, async put() {}, async clear() { cleared++; } },
  });
  controller.start();
  await waitFor(() => controller.status.kind === "recoverable_error" && cleared === 2);
  assert.deepEqual(controller.view().turns, []);
  assert.equal(controller.view().checkpoint, null);
  assert.equal(client.calls.openStreams, 0);
  controller.dispose();
});

test("a hung cache read cannot prevent a network bootstrap", async () => {
  const client = fakeClient({ hub: new StreamQueue() });
  const controller = new ConversationController({ client, agentId: identity.agent_id,
    snapshotCache: { load() { return new Promise(() => {}); }, async store() {} },
  });
  controller.start();
  await waitFor(() => controller.status.kind === "ready");
  assert.equal(client.calls.summary.length, 1);
  controller.dispose();
});

test("access revocation discards late brief responses, but a missing brief preserves the turn", async () => {
  const { ConversationHttpError } = await import("../dist/index.js");
  let resolveLate;
  let persisted = 0;
  const client = fakeClient({ hub: new StreamQueue(), brief(id) {
    if (id === "late") return new Promise((resolve) => { resolveLate = resolve; });
    throw new ConversationHttpError(id === "missing" ? 404 : 403, { ok: false, error: "unavailable" });
  } });
  const controller = new ConversationController({ client, agentId: identity.agent_id,
    briefCache: { async get() {}, async put() { persisted++; }, async clear() {} },
  });
  controller.start();
  await waitFor(() => controller.status.kind === "ready");
  await controller.loadBrief("missing");
  assert.equal(controller.view().turns.length, 1);
  const late = controller.loadBrief("late");
  await waitFor(() => resolveLate);
  await controller.loadBrief("forbidden");
  resolveLate(briefRecord("late"));
  await late;
  assert.deepEqual(controller.view().turns, []);
  assert.equal(controller.briefState("late"), null);
  assert.equal(persisted, 0);
  controller.dispose();
});

test("immediate pause/resume replaces an aborted handshake without stranding the new run", async () => {
  const client = fakeClient({ hub: new StreamQueue() });
  let attempts = 0;
  client.requireCapability = async (signal) => {
    attempts++;
    if (attempts === 1) await new Promise((_, reject) => signal.addEventListener("abort", () => reject(signal.reason), { once: true }));
    return { ok: true };
  };
  const controller = new ConversationController({ client, agentId: identity.agent_id });
  controller.start();
  await waitFor(() => attempts === 1);
  controller.pause();
  controller.start();
  await waitFor(() => client.calls.openStreams === 1);
  assert.equal(controller.status.kind, "ready");
  assert.equal(attempts, 2);
  controller.pause();
  controller.start();
  await waitFor(() => client.calls.stream.length === 2);
  assert.equal(attempts, 2);
  controller.dispose();
});
