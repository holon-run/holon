import assert from "node:assert/strict";
import test from "node:test";

import {
  ConversationProtocolState,
  ConversationStaleResponseError,
  ConversationStateLimitError,
} from "../dist/index.js";
import {
  activity,
  batch,
  detail,
  identity,
  summary,
  turn,
} from "./helpers.mjs";

test("bootstraps, merges older pages, and rejects stale live revisions", () => {
  const state = new ConversationProtocolState();
  state.bootstrap(
    identity,
    summary({
      turns: [turn("turn-10", 10)],
      active_turns: [turn("turn-live", 11)],
    }),
  );
  state.applyBatch(
    identity,
    batch({
      mutations: [
        {
          type: "turn_summary_upsert",
          event_log_epoch: "epoch-a",
          visibility_scope_id: "scope-a",
          turn: turn("turn-live", 11, 2, {
            execution: { kind: "terminal", outcome: "completed" },
            result: { kind: "available" },
            settled: true,
            brief_ids: ["brief-live"],
          }),
        },
      ],
    }),
  );
  state.applyOlderPage(
    identity,
    "history-before-10",
    summary({
      snapshot_through_seq: 11,
      event_head_seq: 11,
      snapshot_cursor: "checkpoint-11",
      turns: [
        turn("turn-1", 1),
        turn("turn-live", 11, 1),
      ],
      active_turns: [],
      next_before_cursor: null,
      has_more: false,
    }),
  );
  const live = state
    .view()
    .turns.find((candidate) => candidate.turn_id === "turn-live");
  assert.equal(live.revision, 2);
  assert.equal(live.execution.kind, "terminal");
  assert.deepEqual(live.brief_ids, ["brief-live"]);
});

test("rejects stale generation and terminal resurrection from late detail", () => {
  const state = new ConversationProtocolState();
  state.bootstrap(identity, summary());
  assert.throws(
    () =>
      state.applyOlderPage(
        { ...identity, generation: 2 },
        "history-before-10",
        summary(),
      ),
    ConversationStaleResponseError,
  );

  state.applyBatch(
    identity,
    batch({
      mutations: [
        {
          type: "turn_summary_upsert",
          event_log_epoch: "epoch-a",
          visibility_scope_id: "scope-a",
          turn: turn("turn-10", 10, 2, {
            execution: { kind: "terminal", outcome: "completed" },
            settled: true,
          }),
        },
      ],
    }),
  );
  state.applyDetailPage(
    identity,
    "turn-10",
    undefined,
    detail({
      snapshot_through_seq: 11,
      event_head_seq: 11,
      snapshot_cursor: "checkpoint-11",
      turn: turn("turn-10", 10, 3),
    }),
  );
  const selected = state.view().turns.find((item) => item.turn_id === "turn-10");
  assert.equal(selected.execution.kind, "terminal");
  assert.equal(selected.revision, 2);
});

test("applies batches atomically when a client-side bound is exceeded", () => {
  const state = new ConversationProtocolState({ max_pending_inputs: 1 });
  state.bootstrap(identity, summary({ pending_inputs: [] }));
  assert.throws(
    () =>
      state.applyBatch(
        identity,
        batch({
          mutations: [
            {
              type: "operator_upsert",
              event_log_epoch: "epoch-a",
              visibility_scope_id: "scope-a",
              input: {
                message_id: "message-1",
                revision: 1,
                state: "queued",
              },
            },
            {
              type: "operator_upsert",
              event_log_epoch: "epoch-a",
              visibility_scope_id: "scope-a",
              input: {
                message_id: "message-2",
                revision: 1,
                state: "queued",
              },
            },
          ],
        }),
      ),
    ConversationStateLimitError,
  );
  const view = state.view();
  assert.deepEqual(view.pending_inputs, []);
  assert.equal(view.checkpoint, "checkpoint-10");
  assert.equal(view.through_seq, 10);
});

test("bounds live activity, invalidates detail, and rejects stale refetch", () => {
  const state = new ConversationProtocolState({
    max_activities_per_turn: 2,
  });
  state.bootstrap(identity, summary());
  state.applyDetailPage(
    identity,
    "turn-10",
    undefined,
    detail({
      detail_revision: 2,
      activities: [
        activity("activity-1", 1),
        activity("activity-2", 2),
        activity("activity-3", 3),
      ],
    }),
  );
  let selected = state.view().details[0];
  assert.deepEqual(
    selected.activities.map((item) => item.id),
    ["activity-2", "activity-3"],
  );
  assert.equal(selected.truncated, true);
  assert.equal(selected.has_more, true);

  state.applyBatch(
    identity,
    batch({
      mutations: [
        {
          type: "detail_invalidated",
          event_log_epoch: "epoch-a",
          visibility_scope_id: "scope-a",
          turn_id: "turn-10",
          detail_revision: 3,
        },
      ],
    }),
  );
  selected = state.view().details[0];
  assert.equal(selected.invalidated, true);
  assert.equal(
    state.applyDetailPage(
      identity,
      "turn-10",
      undefined,
      detail({
        snapshot_through_seq: 11,
        event_head_seq: 11,
        snapshot_cursor: "checkpoint-11",
        detail_revision: 2,
      }),
    ),
    false,
  );
  assert.equal(state.view().details[0].invalidated, true);
});

test("retains unknown active turns only in the bounded live window and resets reconnect", () => {
  const state = new ConversationProtocolState({ max_live_turns: 1 });
  state.bootstrap(identity, summary());
  state.applyBatch(
    identity,
    batch({
      mutations: [
        {
          type: "turn_summary_upsert",
          event_log_epoch: "epoch-a",
          visibility_scope_id: "scope-a",
          turn: turn("unknown-terminal", 20, 1, {
            execution: { kind: "terminal", outcome: "completed" },
          }),
        },
        {
          type: "turn_summary_upsert",
          event_log_epoch: "epoch-a",
          visibility_scope_id: "scope-a",
          turn: turn("unknown-active", 21),
        },
      ],
    }),
  );
  assert.equal(
    state.view().turns.some((item) => item.turn_id === "unknown-terminal"),
    false,
  );
  assert.equal(
    state.view().turns.some((item) => item.turn_id === "unknown-active"),
    true,
  );
  assert.equal(state.reconnectCheckpoint(), "checkpoint-11");
  state.reset("retention_expired");
  assert.equal(state.reconnectCheckpoint(), null);
  assert.equal(state.view().reset_reason, "retention_expired");
  assert.deepEqual(state.view().turns, []);
});
