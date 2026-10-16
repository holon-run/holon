import assert from "node:assert/strict";
import {
  ConversationClient, ConversationHttpError, CONVERSATION_INCARNATION_CAPABILITY,
} from "../dist/index.js";

const baseUrl = process.env.HOLON_CONVERSATION_E2E_BASE_URL;
assert.ok(baseUrl);
const source = new ConversationClient({ baseUrl });
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
  abort.abort();
  await stream.return();
}
console.log("incarnation-bound HTTP/304/stream/resume/delete/recreate passed without identity requests");
