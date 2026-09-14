import assert from "node:assert/strict";
import test from "node:test";

import {
  ConversationProtocolError,
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

test("rejects late pages after stream advances even when revision guards were evicted", () => {
  const terminal = {
    execution: { kind: "terminal", outcome: "completed" },
    result: { kind: "available" },
    settled: true,
  };
  const state = new ConversationProtocolState({
    max_detail_turns: 1,
    max_pending_inputs: 1,
  });
  state.bootstrap(
    identity,
    summary({
      turns: [
        turn("turn-a", 10, 1, terminal),
        turn("turn-b", 9, 1, terminal),
      ],
    }),
  );
  state.applyDetailPage(
    identity,
    "turn-a",
    undefined,
    detail({
      turn: turn("turn-a", 10, 1, terminal),
      detail_revision: 1,
    }),
  );
  state.applyBatch(
    identity,
    batch({
      mutations: [
        {
          type: "detail_invalidated",
          event_log_epoch: "epoch-a",
          visibility_scope_id: "scope-a",
          turn_id: "turn-a",
          detail_revision: 2,
        },
        {
          type: "operator_remove",
          event_log_epoch: "epoch-a",
          visibility_scope_id: "scope-a",
          message_id: "message-a",
          revision: 2,
        },
      ],
    }),
  );
  state.applyDetailPage(
    identity,
    "turn-b",
    undefined,
    detail({
      snapshot_through_seq: 11,
      event_head_seq: 11,
      snapshot_cursor: "checkpoint-11",
      turn: turn("turn-b", 9, 1, terminal),
    }),
  );
  state.applyBatch(
    identity,
    batch({
      from: 11,
      through: 12,
      mutations: [
        {
          type: "operator_remove",
          event_log_epoch: "epoch-a",
          visibility_scope_id: "scope-a",
          message_id: "message-b",
          revision: 2,
        },
      ],
    }),
  );

  assert.throws(
    () =>
      state.applyDetailPage(
        identity,
        "turn-a",
        undefined,
        detail({
          snapshot_through_seq: 10,
          event_head_seq: 10,
          snapshot_cursor: "checkpoint-10",
          turn: turn("turn-a", 10, 1, terminal),
          detail_revision: 1,
        }),
      ),
    ConversationStaleResponseError,
  );
  assert.throws(
    () =>
      state.applyOlderPage(
        identity,
        "history-before-10",
        summary({
          pending_inputs: [
            {
              message_id: "message-a",
              revision: 1,
              state: "queued",
            },
          ],
        }),
      ),
    ConversationStaleResponseError,
  );
  assert.deepEqual(
    state.view().details.map((item) => item.turn_id),
    ["turn-b"],
  );
  assert.deepEqual(state.view().pending_inputs, []);
});

test("rejects checkpoint reuse for a different batch boundary", () => {
  const state = new ConversationProtocolState();
  state.bootstrap(identity, summary());
  assert.equal(
    state.applyBatch(
      identity,
      batch({
        from: 10,
        through: 10,
        checkpoint: "checkpoint-10",
      }),
    ),
    false,
  );
  assert.throws(
    () =>
      state.applyBatch(
        identity,
        batch({
          from: 10,
          through: 11,
          checkpoint: "checkpoint-10",
        }),
      ),
    ConversationProtocolError,
  );
  assert.equal(state.view().through_seq, 10);

  const applied = batch({ from: 10, through: 11 });
  assert.equal(state.applyBatch(identity, applied), true);
  assert.equal(state.applyBatch(identity, applied), false);
  assert.throws(
    () =>
      state.applyBatch(
        identity,
        batch({
          from: 11,
          through: 12,
          checkpoint: "checkpoint-11",
        }),
      ),
    ConversationProtocolError,
  );
  assert.equal(state.view().through_seq, 11);
});

test("older pages extend turn history without replacing pending inputs", () => {
  const state = new ConversationProtocolState();
  state.bootstrap(
    identity,
    summary({
      pending_inputs: [
        {
          message_id: "message-current",
          revision: 2,
          state: "assigning",
        },
      ],
    }),
  );
  state.applyOlderPage(
    identity,
    "history-before-10",
    summary({
      pending_inputs: [
        {
          message_id: "message-stale",
          revision: 1,
          state: "queued",
        },
      ],
      turns: [turn("turn-old", 1)],
      next_before_cursor: null,
      has_more: false,
    }),
  );
  assert.deepEqual(state.view().pending_inputs, [
    {
      message_id: "message-current",
      revision: 2,
      state: "assigning",
    },
  ]);
});

test("marks an uncached detail invalidated on its first invalidation", () => {
  const state = new ConversationProtocolState();
  state.bootstrap(identity, summary());
  state.applyBatch(
    identity,
    batch({
      mutations: [
        {
          type: "detail_invalidated",
          event_log_epoch: "epoch-a",
          visibility_scope_id: "scope-a",
          turn_id: "turn-10",
          detail_revision: 2,
        },
      ],
    }),
  );
  assert.deepEqual(state.view().details, [
    {
      turn_id: "turn-10",
      detail_revision: 2,
      activities: [],
      coverage: { kind: "unknown" },
      next_before_cursor: null,
      has_more: false,
      invalidated: true,
      truncated: false,
    },
  ]);
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
