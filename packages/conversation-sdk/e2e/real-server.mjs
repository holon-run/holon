import assert from "node:assert/strict";

import {
  ConversationClient,
  ConversationProtocolState,
  ConversationResetError,
} from "../dist/index.js";

const baseUrl = process.env.HOLON_CONVERSATION_E2E_BASE_URL;
const controlUrl = process.env.HOLON_CONVERSATION_E2E_CONTROL_URL;
assert.ok(baseUrl, "HOLON_CONVERSATION_E2E_BASE_URL is required");
assert.ok(controlUrl, "HOLON_CONVERSATION_E2E_CONTROL_URL is required");

const identity = {
  remote_id: baseUrl,
  agent_id: "web",
  generation: 1,
};

async function control(action) {
  const response = await fetch(controlUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ action }),
  });
  if (!response.ok) {
    assert.fail(`control ${action} failed with ${response.status}: ${await response.text()}`);
  }
  return response.json();
}

async function firstStreamItem(stream) {
  for await (const item of stream) {
    return item;
  }
  throw new Error("conversation stream closed before yielding an item");
}

function truncateBeforeCheckpoint(response) {
  assert.ok(response.body, "stream response must have a body");
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  const encoder = new TextEncoder();
  let buffered = "";
  return new Response(
    new ReadableStream({
      async pull(controller) {
        while (true) {
          const { done, value } = await reader.read();
          if (done) {
            if (buffered.length > 0) {
              controller.enqueue(encoder.encode(buffered));
            }
            controller.close();
            return;
          }
          buffered += decoder.decode(value, { stream: true });
          const checkpoint = buffered.indexOf("event: checkpoint");
          if (checkpoint !== -1) {
            const partial = buffered.slice(0, checkpoint);
            if (partial.length > 0) {
              controller.enqueue(encoder.encode(partial));
            }
            controller.close();
            await reader.cancel();
            return;
          }
        }
      },
      async cancel(reason) {
        await reader.cancel(reason);
      },
    }),
    {
      status: response.status,
      statusText: response.statusText,
      headers: response.headers,
    },
  );
}

async function withTimeout(promise, message) {
  return Promise.race([
    promise,
    new Promise((_, reject) =>
      setTimeout(() => reject(new Error(message)), 10_000),
    ),
  ]);
}

const client = new ConversationClient({ baseUrl });
await client.requireCapability();

const state = new ConversationProtocolState();
const { summary: snapshot } = await client.summary("web", { limit: 2 });
assert.ok(snapshot, "unconditional summary must return a snapshot body");
assert.equal(snapshot.turns.length, 2);
assert.equal(snapshot.has_more, true);
assert.ok(snapshot.next_before_cursor);
state.bootstrap(identity, snapshot);

await control("late_brief");
const attach = await firstStreamItem(
  client.stream("web", {
    after: snapshot.snapshot_cursor,
    cursorTransport: "query",
  }),
);
assert.equal(attach.type, "batch");
assert.equal(state.applyBatch(identity, attach.batch), true);
const resultTurn = state
  .view()
  .turns.find((turn) => turn.turn_id === "turn-result");
assert.ok(resultTurn);
assert.ok(resultTurn.brief_ids.includes("brief-late"));

const olderCursor = state.view().next_before_cursor;
assert.ok(olderCursor);
const { summary: older } = await client.summary("web", {
  limit: 2,
  before: olderCursor,
});
assert.ok(older, "unconditional older page must return a snapshot body");
state.applyOlderPage(identity, olderCursor, older);
assert.deepEqual(
  state.view().turns.map((turn) => turn.turn_id),
  ["turn-oldest", "turn-zero-brief", "turn-result", "turn-active"],
);

const detail = await client.activities("web", "turn-active", { limit: 20 });
state.applyDetailPage(identity, "turn-active", undefined, detail);
assert.ok(detail.activities.length >= 2);
assert.equal(
  JSON.stringify(detail).includes("must-not-leak"),
  false,
  "bounded detail must not expose raw tool input/output",
);
const brief = await client.brief("web", "brief-late");
assert.equal(brief.text, "late deferred brief body");

await control("finish_active");
const checkpointBeforePartial = state.reconnectCheckpoint();
assert.ok(checkpointBeforePartial);
let truncateOnce = true;
const partialClient = new ConversationClient({
  baseUrl,
  fetch: async (input, init) => {
    const response = await fetch(input, init);
    if (
      truncateOnce &&
      String(input).includes("/conversation/stream") &&
      response.ok
    ) {
      truncateOnce = false;
      return truncateBeforeCheckpoint(response);
    }
    return response;
  },
});
const partialItems = [];
for await (const item of partialClient.stream("web", {
  after: checkpointBeforePartial,
})) {
  partialItems.push(item);
}
assert.deepEqual(partialItems, []);
assert.equal(state.reconnectCheckpoint(), checkpointBeforePartial);

const replayed = await firstStreamItem(
  client.stream("web", { after: checkpointBeforePartial }),
);
assert.equal(replayed.type, "batch");
state.applyBatch(identity, replayed.batch);
const completed = state
  .view()
  .turns.find((turn) => turn.turn_id === "turn-active");
assert.equal(completed.execution.kind, "terminal");

const expiredCheckpoint = state.reconnectCheckpoint();
assert.ok(expiredCheckpoint);
await control("retention");
await assert.rejects(
  firstStreamItem(client.stream("web", { after: expiredCheckpoint })),
  (error) =>
    error instanceof ConversationResetError &&
    error.reason === "retention_expired",
);
state.reset("retention_expired");
assert.equal(state.reconnectCheckpoint(), null);

const { summary: recovered } = await client.summary("web", { limit: 2 });
assert.ok(recovered, "post-reset summary must return a snapshot body");
state.bootstrap(identity, recovered);
const iterator = client
  .stream("web", { after: recovered.snapshot_cursor })
  [Symbol.asyncIterator]();
const initial = await withTimeout(
  iterator.next(),
  "initial post-reset stream batch timed out",
);
assert.equal(initial.done, false);
assert.equal(initial.value.type, "batch");
state.applyBatch(identity, initial.value.batch);

const waiting = iterator.next();
await control("burst");
let next = await withTimeout(waiting, "backpressure stream result timed out");
let reset = null;
for (let attempts = 0; attempts < 4 && !next.done; attempts += 1) {
  if (next.value.type === "batch") {
    state.applyBatch(identity, next.value.batch);
    next = await withTimeout(
      iterator.next(),
      "backpressure reset did not follow the recovery batch",
    );
    continue;
  }
  reset = next.value.reset;
  break;
}
assert.ok(reset, "backpressure must emit reset_required");
assert.equal(reset.reason, "slow_consumer");
state.reset(reset.reason);
await iterator.return();

console.log(
  JSON.stringify({
    ok: true,
    scenarios: [
      "snapshot_attach",
      "older_page",
      "detail",
      "deferred_brief",
      "mid_batch_reconnect",
      "retention_reset",
      "backpressure_reset",
    ],
  }),
);
