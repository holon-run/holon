import assert from "node:assert/strict";
import {
  ConversationClient, ConversationController, ConversationHttpError, ConversationResetError,
  CONVERSATION_INCARNATION_CAPABILITY,
} from "../dist/index.js";

const baseUrl = process.env.HOLON_CONVERSATION_E2E_BASE_URL;
assert.ok(baseUrl);
let unboundStreams = 0;
const source = new ConversationClient({
  baseUrl,
  fetch: async (url, init) => {
    const response = await fetch(url, init);
    if (new URL(url).pathname.endsWith("/stream") &&
        !new Headers(init.headers).has("x-holon-expected-incarnation")) unboundStreams++;
    return response;
  },
});
assert.ok((await source.requireCapability()).capabilities.includes(CONVERSATION_INCARNATION_CAPABILITY));
const bound = () => source.withIncarnationFence({ agentId: "web", incarnation: 1 });
const client = bound();
const snapshot = await client.summary("web", { limit: 1 });
assert.ok(snapshot.summary && snapshot.etag);
assert.equal((await client.summary("web", { ifNoneMatch: snapshot.etag, limit: 1 })).summary, null);
await client.activities("web", "turn-active");
const originalBrief = await client.brief("web", "brief-one");
await client.locateSource("web", "brief:brief-one", { contextLimit: 1 });

// A complete initial checkpoint is admitted under incarnation 1.
const abort = new AbortController();
const stream = client.stream("web", { after: snapshot.summary.snapshot_cursor, signal: abort.signal });
assert.equal((await stream.next()).value.type, "batch");

// Unnegotiated clients must receive the legacy reset, not a new enum value.
const legacyStream = source.stream("web", { after: snapshot.summary.snapshot_cursor, signal: abort.signal });
assert.equal((await legacyStream.next()).value.type, "batch");
let cachedSnapshot = { summary: snapshot.summary, etag: snapshot.etag };
const cachedBriefs = new Map();
let cacheClears = 0;
const controller = new ConversationController({
  client: source, agentId: "web",
  snapshotCache: {
    async load() { return cachedSnapshot; },
    async store(value) { cachedSnapshot = value; },
    async clear() { cachedSnapshot = null; cacheClears++; },
  },
  briefCache: {
    async get(id) { return cachedBriefs.get(id); },
    async put(id, value) { cachedBriefs.set(id, value); },
    async clear() { cachedBriefs.clear(); cacheClears++; },
  },
});
const until = async (predicate, label) => {
  const deadline = Date.now() + 20_000;
  while (!predicate()) {
    assert.ok(Date.now() < deadline, label);
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
};
controller.start();
await until(() => unboundStreams === 2, "unbound Controller did not attach");
await controller.loadBrief("brief-one");
assert.ok(controller.view().turns.length > 0);
assert.equal(cachedBriefs.size, 1);

const request = async (path, method, body) => {
  const response = await fetch(`${baseUrl}${path}`, {
    method, ...(body === undefined ? {} : { headers: { "content-type": "application/json" }, body: JSON.stringify(body) }),
  });
  assert.ok(response.ok, `${method} ${path}: ${response.status} ${await (!response.ok ? response.text() : "")}`);
  return response.json();
};
try {
  await request("/control/agents/web", "DELETE", {});
  const deadline = Date.now() + 20_000;
  while ((await request("/control/agents/web/delete-status", "GET")).job.status !== "completed") {
    assert.ok(Date.now() < deadline, "real deletion did not complete");
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  const recreated = await request("/control/agents/web/create", "POST", {});
  assert.equal(recreated.identity.incarnation, 2);
  const isIdentityError = (error) => error instanceof ConversationHttpError &&
    error.body.code === "conversation_identity_changed" && error.body.retryable === false;
  // The old live stream terminates; it cannot recover under the replacement identity.
  await assert.rejects(stream.next(), isIdentityError);
  const legacyReset = await legacyStream.next();
  assert.equal(legacyReset.value.type, "reset_required");
  assert.equal(legacyReset.value.reset.reason, "agent_not_found");
  assert.equal((await legacyStream.next()).done, true);
  await until(() => cacheClears === 2, "unbound Controller did not clear deleted identity");
  assert.equal(controller.status.kind, "recoverable_error");
  assert.ok(controller.status.error instanceof ConversationResetError);
  assert.equal(controller.status.error.reason, "agent_not_found");
  assert.deepEqual(controller.view().turns, []);
  assert.equal(controller.view().checkpoint, null);
  assert.equal(controller.briefState("brief-one"), null);
  assert.equal(cachedSnapshot, null);
  assert.equal(cachedBriefs.size, 0);
  assert.equal(unboundStreams, 2, "an unbound Controller must not follow the replacement");
  for (const operation of [
    (pinned) => pinned.summary("web"),
    (pinned) => pinned.summary("web", { ifNoneMatch: snapshot.etag }),
    (pinned) => pinned.summary("web", { before: snapshot.summary.next_before_cursor }),
    (pinned) => pinned.activities("web", "turn-active"),
    (pinned) => pinned.brief("web", "brief-one"),
    (pinned) => pinned.locateSource("web", "brief:brief-one"),
    (pinned) => pinned.stream("web", { after: snapshot.summary.snapshot_cursor }).next(),
  ]) await assert.rejects(operation(bound()), isIdentityError);
  const fresh = source.withIncarnationFence({ agentId: "web", incarnation: 2 });
  // Deletion retains durable history; assert the binding, not an empty projection.
  const freshSnapshot = await fresh.summary("web");
  assert.ok(freshSnapshot.summary);
  assert.ok(freshSnapshot.summary.turns.some((turn) => turn.turn_id === "turn-active"),
    "newly admitted reads still project retained pre-deletion turns");
  assert.deepEqual(await fresh.brief("web", "brief-one"), originalBrief,
    "newly admitted reads still expose the retained pre-deletion Brief without rewriting it");
  assert.notEqual(freshSnapshot.summary.snapshot_cursor, snapshot.summary.snapshot_cursor);
  await assert.rejects(fresh.stream("web", { after: snapshot.summary.snapshot_cursor }).next(),
    (error) => error instanceof ConversationHttpError &&
      error.body.code === "conversation_cursor_scope_mismatch");
} finally {
  controller.dispose();
  abort.abort();
  await stream.return();
  await legacyStream.return();
}
console.log("incarnation-bound HTTP/304/stream/resume/delete/recreate and legacy Controller cache invalidation passed without identity requests");
