import assert from "node:assert/strict";
import test from "node:test";
import {
  ConversationClient, ConversationHttpError, ConversationCapabilityError,
  CONVERSATION_INCARNATION_CAPABILITY,
} from "../dist/index.js";
import { summary, detail, batch } from "./helpers.mjs";

const identity = { agentId: "web", incarnation: 1 };
const headers = {
  "x-holon-agent-id": "web",
  "x-holon-agent-incarnation": "1",
};
const handshake = (capabilities = [CONVERSATION_INCARNATION_CAPABILITY]) => ({
  ok: true, protocol: { name: "holon-control", version: 1 },
  auth: { mode: "none", required: false }, runtime: {},
  capabilities: ["agents.conversation-read.v1", "agents.conversation-locate.v1", ...capabilities],
});
const brief = {
  id: "brief", agent_id: "web", workspace_id: "workspace", kind: "result", text: "done",
  created_at: "2026-10-16T00:00:00Z",
  related_message_id: null, related_task_id: null,
};
function clientFor(read, capabilities) {
  return new ConversationClient({
    baseUrl: "http://runtime.test/api", expectedIdentity: identity,
    fetch: async (url, init) => {
      if (String(url).endsWith("/handshake")) return Response.json(handshake(capabilities));
      assert.equal(new Headers(init.headers).get("x-holon-expected-incarnation"), "1");
      return read(url, init);
    },
  });
}
const operations = [
  ["summary", (client) => client.summary("web"), summary()],
  ["activities", (client) => client.activities("web", "turn-10"), detail()],
  ["brief", (client) => client.brief("web", "brief"), brief],
  ["locate", (client) => client.locateSource("web", "turn:turn-10"), {
    schema_version: 2, query_version: 2, agent_id: "web",
    status: "source_only", source_ref: "turn:turn-10", target: null,
    reason: "unsupported_conversation_source", runtime_id: "runtime-a",
    event_log_epoch: "epoch-a", visibility_scope_id: "scope-a", window: null,
  }],
];
for (const [name, invoke, value] of operations) {
  test(`${name} validates binding after JSON decode and latches invalidation`, async () => {
    let release;
    const client = clientFor(async () => {
      const response = Response.json(value, { headers });
      response.json = () => new Promise((resolve) => { release = () => resolve(value); });
      return response;
    });
    const pending = invoke(client);
    while (!release) await new Promise((resolve) => setImmediate(resolve));
    // The identity belongs to the response's transaction, not a later identity lookup.
    release();
    await pending;
    const invalid = clientFor(async () => Response.json(value));
    await assert.rejects(invoke(invalid), ConversationHttpError);
    await assert.rejects(invalid.summary("web"), /identity binding/);
  });
}

for (const supplied of [{}, { ...headers, "x-holon-agent-id": "other" },
  { ...headers, "x-holon-agent-incarnation": "2" },
  { ...headers, "x-holon-agent-incarnation": "01" }]) {
  test(`conditional 304 rejects missing/wrong binding ${JSON.stringify(supplied)}`, async () => {
    const client = clientFor(async () => new Response(null, { status: 304, headers: supplied }));
    await assert.rejects(client.summary("web", { ifNoneMatch: '"etag"' }), ConversationHttpError);
  });
}
test("bound 304 is valid without a JSON body", async () => {
  const client = clientFor(async () => new Response(null, { status: 304, headers }));
  assert.equal((await client.summary("web", { ifNoneMatch: '"etag"' })).summary, null);
});
test("unknown capability cannot enable bound reads", async () => {
  let reads = 0;
  const client = clientFor(async () => { reads++; return Response.json(summary(), { headers }); },
    ["agents.conversation-incarnation.v999"]);
  await assert.rejects(client.summary("web"), ConversationCapabilityError);
  assert.equal(reads, 0);
});

const boundFrame = (frame) => ({ ...frame, agent_id: "web", agent_incarnation: 1 });
function streamResponse(frames) {
  const text = frames.map((frame) => `event: ${frame.type}\n${
    frame.type === "checkpoint" ? `id: ${frame.checkpoint}\n` : ""
  }data: ${JSON.stringify(frame)}\n\n`).join("");
  return new Response(text, { headers });
}
for (const unit of ["batch_begin", "operator_remove", "checkpoint", "reset_required"]) {
  for (const defect of ["missing", "replacement"]) {
    test(`stream rejects ${defect} binding on ${unit} before yielding/checkpoint`, async () => {
      const change = { type: "operator_remove", message_id: "input", revision: 1 };
      const value = batch({ mutations: [change] });
      const frames = unit === "reset_required"
        ? [{ type: "reset_required", reason: "retention_expired", hint: "reset",
          oldest_retained_seq: 1, event_head_seq: 11 }]
        : [value.begin, change, value.checkpoint];
      const wire = frames.map((frame) => {
        if (frame.type !== unit) return boundFrame(frame);
        return defect === "missing" ? frame : { ...boundFrame(frame), agent_incarnation: 2 };
      });
      const client = clientFor(async () => streamResponse(wire));
      await assert.rejects(client.stream("web").next(), ConversationHttpError);
      await assert.rejects(client.summary("web"), ConversationHttpError);
    });
  }
}
test("many stream batches need no identity HTTP requests", async () => {
  let requests = 0;
  const frames = Array.from({ length: 100 }, (_, index) => {
    const value = batch({ from: index, through: index + 1 });
    return [value.begin, value.checkpoint].map(boundFrame);
  }).flat();
  const client = clientFor(async () => { requests++; return streamResponse(frames); });
  let count = 0;
  for await (const _item of client.stream("web")) count++;
  assert.equal(count, 100);
  assert.equal(requests, 1);
});
test("identity-changed reset is terminal, even with the old valid binding", async () => {
  const client = clientFor(async () => streamResponse([boundFrame({
    type: "reset_required", reason: "agent_identity_changed", hint: "replaced",
    oldest_retained_seq: null, event_head_seq: null,
  })]));
  await assert.rejects(client.stream("web").next(), ConversationHttpError);
});
test("aborted decoding and concurrent invalidation cannot release a result", async () => {
  let release;
  const abort = new AbortController();
  const client = clientFor(async (url) => {
    if (String(url).endsWith("/briefs/brief")) return Response.json(brief);
    const response = Response.json(summary(), { headers });
    response.json = () => new Promise((resolve) => { release = () => resolve(summary()); });
    return response;
  });
  const pending = client.summary("web", { signal: abort.signal });
  while (!release) await new Promise((resolve) => setImmediate(resolve));
  await assert.rejects(client.brief("web", "brief"), ConversationHttpError);
  const rejected = assert.rejects(pending, ConversationHttpError);
  release();
  await rejected;
  const aborted = clientFor(async () => {
    const response = Response.json(summary(), { headers });
    response.json = async () => { abort.abort(); return summary(); };
    return response;
  });
  await assert.rejects(aborted.summary("web", { signal: abort.signal }), { name: "AbortError" });
});
