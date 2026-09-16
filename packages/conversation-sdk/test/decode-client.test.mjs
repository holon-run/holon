import assert from "node:assert/strict";
import test from "node:test";

import {
  CONVERSATION_CAPABILITY,
  ConversationCapabilityError,
  ConversationClient,
  ConversationCompatibilityError,
  ConversationDecodeError,
  ConversationResetError,
  decodeBriefRecord,
  decodeConversationActivity,
  decodeConversationSummaryResponse,
} from "../dist/index.js";
import { summary, turn } from "./helpers.mjs";

test("decodes pending input previews and defaults missing previews to empty", () => {
  const decoded = decodeConversationSummaryResponse(
    summary({
      pending_inputs: [
        {
          message_id: "message-pending",
          revision: 2,
          state: "assigning",
          preview: '{"type":"text","text":"check this"}',
        },
        {
          message_id: "message-legacy",
          revision: 1,
          state: "queued",
        },
        {
          message_id: "message-empty",
          revision: 3,
          state: "queued",
          preview: "",
        },
      ],
    }),
  );
  assert.deepEqual(decoded.pending_inputs, [
    {
      message_id: "message-pending",
      revision: 2,
      state: "assigning",
      preview: '{"type":"text","text":"check this"}',
    },
    {
      message_id: "message-legacy",
      revision: 1,
      state: "queued",
      preview: "",
    },
    {
      message_id: "message-empty",
      revision: 3,
      state: "queued",
      preview: "",
    },
  ]);
});

test("decodes turn input previews that are present but empty", () => {
  const decoded = decodeConversationSummaryResponse(
    summary({
      turns: [
        turn("turn-empty-preview", 3, 1, {
          inputs: [
            { message_id: "message-text", preview: "operator text" },
            { message_id: "message-blank", preview: "" },
          ],
        }),
      ],
    }),
  );
  assert.deepEqual(decoded.turns[0].inputs, [
    { message_id: "message-text", preview: "operator text" },
    { message_id: "message-blank", preview: "" },
  ]);
});

test("decodes bounded conversation responses and rejects unsafe u64 values", () => {
  const decoded = decodeConversationSummaryResponse(summary());
  assert.equal(decoded.snapshot_cursor, "checkpoint-10");
  assert.equal(decoded.next_before_cursor, "history-before-10");
  assert.equal(decoded.turns[0].turn_id, "turn-10");

  const unsafe = summary({
    turns: [
      {
        ...summary().turns[0],
        revision: Number.MAX_SAFE_INTEGER + 1,
      },
    ],
  });
  assert.throws(
    () => decodeConversationSummaryResponse(unsafe),
    (error) =>
      error instanceof ConversationDecodeError &&
      error.path === "$.turns[0].revision",
  );

  assert.throws(
    () =>
      decodeConversationSummaryResponse(
        summary({
          turns: [
            {
              ...summary().turns[0],
              key: { turn_index: 10, turn_id: "different-turn" },
            },
          ],
        }),
      ),
    (error) =>
      error instanceof ConversationDecodeError &&
      error.path === "$.turns[0].key.turn_id",
  );

  assert.throws(
    () =>
      decodeConversationActivity({
        kind: "assistant",
        id: "activity-a",
        key: { event_seq: 10, activity_id: "activity-b" },
        revision: 1,
        summary: "activity",
      }),
    (error) =>
      error instanceof ConversationDecodeError &&
      error.path === "$.key.activity_id",
  );
});

test("decodes deferred brief bodies without following transcript linkage", () => {
  const decoded = decodeBriefRecord({
    id: "brief-a",
    agent_id: "web",
    workspace_id: "agent_home:web",
    turn_index: 10,
    turn_id: "turn-10",
    kind: "result",
    created_at: "2026-09-14T00:00:00Z",
    content_source: {
      kind: "transcript_entry",
      entry_id: "transcript-a",
      relation: "finalizes",
    },
    text: "bounded result",
    citations: null,
    attachments: null,
    related_message_id: null,
    related_task_id: null,
  });
  assert.equal(decoded.text, "bounded result");
  assert.deepEqual(decoded.content_source, {
    kind: "transcript_entry",
    entry_id: "transcript-a",
    relation: "finalizes",
  });
});

test("injects fetch, base URL, bearer auth, and cursor transport", async () => {
  const requests = [];
  const fetch = async (input, init) => {
    requests.push({
      url: String(input),
      headers: new Headers(init?.headers),
    });
    return Response.json(summary());
  };
  const client = new ConversationClient({
    baseUrl: "http://runtime.test/api/",
    fetch,
    bearerToken: async () => "secret",
    headers: () => ({ "x-test-principal": "sdk" }),
  });
  await client.summary("web agent", {
    limit: 20,
    before: "opaque-history",
  });
  assert.equal(
    requests[0].url,
    "http://runtime.test/api/agents/web%20agent/conversation?limit=20&before=opaque-history",
  );
  assert.equal(requests[0].headers.get("authorization"), "Bearer secret");
  assert.equal(requests[0].headers.get("x-test-principal"), "sdk");
});

test("fails closed on incompatible handshakes, capability absence, and typed resets", async () => {
  for (const protocol of [
    { name: "other-control", version: 1 },
    { name: "holon-control", version: 2 },
  ]) {
    const incompatible = new ConversationClient({
      baseUrl: "http://runtime.test/api",
      fetch: async () =>
        Response.json({
          ok: true,
          protocol,
          auth: { mode: "local", required: false },
          capabilities: [CONVERSATION_CAPABILITY],
          runtime: {},
        }),
    });
    await assert.rejects(
      incompatible.requireCapability(),
      (error) =>
        error instanceof ConversationCompatibilityError &&
        error.protocolName === protocol.name &&
        error.protocolVersion === protocol.version,
    );
  }

  const missing = new ConversationClient({
    baseUrl: "http://runtime.test/api",
    fetch: async () =>
      Response.json({
        ok: true,
        protocol: { name: "holon-control", version: 1 },
        auth: { mode: "local", required: false },
        capabilities: [],
        runtime: {},
      }),
  });
  await assert.rejects(
    missing.requireCapability(),
    (error) =>
      error instanceof ConversationCapabilityError &&
      error.capability === CONVERSATION_CAPABILITY,
  );

  const reset = new ConversationClient({
    baseUrl: "http://runtime.test/api",
    fetch: async () =>
      Response.json(
        {
          ok: false,
          error: "conversation recovery reset required",
          code: "conversation_reset_required",
          reason: "retention_expired",
          oldest_retained_seq: 50,
          event_head_seq: 100,
          hint: "bootstrap a fresh conversation snapshot",
        },
        { status: 409 },
      ),
  });
  await assert.rejects(
    reset.summary("web"),
    (error) =>
      error instanceof ConversationResetError &&
      error.reason === "retention_expired" &&
      error.oldestRetainedSeq === 50 &&
      error.eventHeadSeq === 100,
  );

  for (const malformed of [-1, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
    const invalidReset = new ConversationClient({
      baseUrl: "http://runtime.test/api",
      fetch: async () =>
        Response.json(
          {
            ok: false,
            error: "conversation recovery reset required",
            code: "conversation_reset_required",
            reason: "retention_expired",
            oldest_retained_seq: malformed,
            event_head_seq: 100,
          },
          { status: 409 },
        ),
    });
    await assert.rejects(
      invalidReset.summary("web"),
      (error) =>
        error instanceof ConversationDecodeError &&
        error.path === "$response.oldest_retained_seq",
    );
  }
});

test("decodes canonical turn timing and tolerates older runtimes without it", () => {
  const record = turn("timed", 1, 1, {
    started_at: "2026-09-16T00:00:00Z",
    completed_at: "2026-09-16T00:01:23Z",
    duration_ms: 83000,
    execution: { kind: "terminal", outcome: "completed" },
  });
  const decode = (value) => decodeConversationSummaryResponse(summary({ turns: [value] })).turns[0];
  assert.equal(decode(record).started_at, record.started_at);
  assert.equal(decode(record).completed_at, record.completed_at);
  assert.equal(decode(record).duration_ms, 83000);
  const legacy = decode(turn("old", 1));
  assert.equal(legacy.started_at, null);
  assert.equal(legacy.completed_at, null);
  assert.equal(legacy.duration_ms, null);
  for (const invalid of [{ started_at: "invalid" }, { completed_at: 123 }, { duration_ms: -1 }, { duration_ms: Number.MAX_SAFE_INTEGER + 1 }]) {
    assert.throws(() => decode({ ...record, ...invalid }), ConversationDecodeError);
  }
});
