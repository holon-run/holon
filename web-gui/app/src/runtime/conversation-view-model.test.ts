import { describe, expect, it } from "vitest";

import type {
  ConversationCheckpoint,
  ConversationHistoryCursor,
  ConversationStateView,
  ConversationTurnSummary,
} from "@holon/conversation-sdk";

import {
  buildConversationSessionModel,
  turnResultPresentation,
  turnExecutionPresentation,
  type ConversationTurnGroup,
} from "./conversation-view-model";

function turnSummary(
  turnId: string,
  turnIndex: number,
  overrides: Partial<ConversationTurnSummary> = {},
): ConversationTurnSummary {
  return {
    turn_id: turnId,
    key: { turn_index: turnIndex, turn_id: turnId },
    revision: 1,
    presentation_class: "operator",
    inputs: [],
    execution: { kind: "active" },
    result: { kind: "pending" },
    settled: false,
    attention: null,
    detail_coverage: { kind: "complete" },
    brief_ids: [],
    ...overrides,
  };
}

function stateView(
  turns: ConversationTurnSummary[],
  overrides: Partial<ConversationStateView> = {},
): ConversationStateView {
  return {
    scope: {
      remote_id: "local",
      agent_id: "web",
      generation: 1,
      runtime_id: "runtime-a",
      event_log_epoch: "epoch-a",
      visibility_scope_id: "scope-a",
      schema_version: 1,
      query_version: 1,
    },
    turns,
    pending_inputs: [],
    details: [],
    next_before_cursor: null,
    has_more: false,
    checkpoint: "checkpoint-10" as ConversationCheckpoint,
    through_seq: 10,
    reset_reason: null,
    ...overrides,
  };
}

const group = (turn: ConversationTurnSummary): ConversationTurnGroup => ({
  turnId: turn.turn_id,
  turnIndex: turn.key.turn_index,
  revision: turn.revision,
  presentationClass: turn.presentation_class,
  inputs: turn.inputs,
  execution: turn.execution,
  result: turn.result,
  settled: turn.settled,
  attention: turn.attention,
  detailCoverage: turn.detail_coverage,
  briefIds: turn.brief_ids,
  detail: null,
});

describe("buildConversationSessionModel", () => {
  it("hides settled task receipts without losing canonical history, paging or input assignment", () => {
    const receipt = turnSummary("receipt", 1, {
      presentation_class: "task",
      inputs: [{ message_id: "receipt-input", preview: "Task completed" }],
      execution: { kind: "terminal", outcome: "completed" },
      result: { kind: "none", reason: { kind: "reducer_only", reason: "task_result_without_model_reentry" } },
      settled: true,
    });
    const preservedCases: Partial<ConversationTurnSummary>[] = [
      { execution: { kind: "active" }, result: { kind: "pending" }, settled: false, duration_ms: 0 },
      { brief_ids: ["brief-1"] },
      { execution: { kind: "terminal", outcome: "provider_failed_needs_recovery" } },
      { attention: { kind: "waiting" } },
      { settled: false },
      { presentation_class: "operator" },
      { result: { kind: "none", reason: { kind: "tool_only_wait" } } },
    ];
    const retained = preservedCases.map((overrides, index) => ({
      ...receipt, ...overrides, turn_id: `retained-${index}`,
      key: { turn_index: index + 2, turn_id: `retained-${index}` },
    }));
    const view = stateView([receipt, ...retained], {
      pending_inputs: [{ message_id: "receipt-input", revision: 1, state: "queued", preview: "Task completed", presentation_class: "task" }],
      has_more: true,
      next_before_cursor: "older" as ConversationHistoryCursor,
    });
    const model = buildConversationSessionModel({
      status: { kind: "ready" }, view, historyState: { kind: "idle" },
      briefs: new Map(), briefLoadStates: new Map(), detailLoadStates: new Map(),
    });
    expect(model.turns.map((turn) => turn.turnId)).toEqual(retained.map((turn) => turn.turn_id));
    expect(model.activeTurn?.turnId).toBe(retained[0].turn_id);
    expect(model.pendingInputs).toEqual([]);
    expect(model.view).toBe(view);
    expect(model.view?.turns).toHaveLength(8);
    expect(model.hasMoreHistory).toBe(true);
    expect(model.view?.next_before_cursor).toBe("older");
  });

  it("maps turns in ascending order and attaches matching details", () => {
    const view = stateView(
      [turnSummary("turn-2", 2), turnSummary("turn-1", 1)],
      {
        details: [
          {
            turn_id: "turn-2",
            detail_revision: 1,
            activities: [],
            coverage: { kind: "complete" },
            next_before_cursor: null,
            has_more: false,
            invalidated: false,
            truncated: false,
          },
        ],
      },
    );
    const model = buildConversationSessionModel({
      status: { kind: "ready" },
      view,
      historyState: { kind: "idle" },
      briefs: new Map(),
      briefLoadStates: new Map(),
      detailLoadStates: new Map(),
    });
    expect(model.turns.map((turn) => turn.turnId)).toEqual([
      "turn-1",
      "turn-2",
    ]);
    expect(model.turns[0]?.detail).toBeNull();
    expect(model.turns[1]?.detail?.turn_id).toBe("turn-2");
    expect(model.activeTurn?.turnId).toBe("turn-2");
  });

  it("treats a missing view before bootstrap as loading", () => {
    const model = buildConversationSessionModel({
      status: { kind: "loading" },
      view: null,
      historyState: { kind: "idle" },
      briefs: new Map(),
      briefLoadStates: new Map(),
      detailLoadStates: new Map(),
    });
    expect(model.bootstrapLoading).toBe(true);
    expect(model.turns).toEqual([]);
  });

  it("treats a scope-less view before bootstrap as loading", () => {
    // The SDK emits a non-null empty view object before the first
    // bootstrap; the pane must render the loading state rather than a
    // blank surface behind the sync banner on a cold cache miss.
    const model = buildConversationSessionModel({
      status: { kind: "loading" },
      view: stateView([], { scope: null }),
      historyState: { kind: "idle" },
      briefs: new Map(),
      briefLoadStates: new Map(),
      detailLoadStates: new Map(),
    });
    expect(model.bootstrapLoading).toBe(true);
    expect(model.turns).toEqual([]);
  });

  it("keeps a scoped view during revalidation out of the loading state", () => {
    const model = buildConversationSessionModel({
      status: { kind: "loading" },
      view: stateView([turnSummary("turn-1", 1)]),
      historyState: { kind: "idle" },
      briefs: new Map(),
      briefLoadStates: new Map(),
      detailLoadStates: new Map(),
    });
    expect(model.bootstrapLoading).toBe(false);
    expect(model.turns.map((turn) => turn.turnId)).toEqual(["turn-1"]);
  });

  it("carries pending inputs and history paging state", () => {
    const model = buildConversationSessionModel({
      status: { kind: "ready" },
      view: stateView([], {
        pending_inputs: [
          { message_id: "m-1", revision: 1, state: "queued", preview: "" },
        ],
        next_before_cursor: "history-cursor" as ConversationHistoryCursor,
        has_more: true,
      }),
      historyState: { kind: "loading" },
      briefs: new Map(),
      briefLoadStates: new Map(),
      detailLoadStates: new Map(),
    });
    expect(model.pendingInputs).toHaveLength(1);
    expect(model.hasMoreHistory).toBe(true);
    expect(model.historyState.kind).toBe("loading");
  });
});

describe("turnResultPresentation", () => {
  it("classifies active turns as pending", () => {
    expect(turnResultPresentation(group(turnSummary("t", 1))).kind).toBe(
      "pending",
    );
  });

  it("prefers available briefs over other result records", () => {
    const presentation = turnResultPresentation(
      group(
        turnSummary("t", 1, {
          execution: { kind: "terminal", outcome: "completed" },
          result: { kind: "available" },
          settled: true,
          brief_ids: ["brief-1", "brief-2"],
        }),
      ),
    );
    expect(presentation).toEqual({
      kind: "available",
      briefIds: ["brief-1", "brief-2"],
    });
  });

  it("surfaces retention gaps as unavailable with retryability", () => {
    const presentation = turnResultPresentation(
      group(
        turnSummary("t", 1, {
          execution: { kind: "terminal", outcome: "completed" },
          result: {
            kind: "unavailable",
            reason: "retention_gap",
            retryable: false,
          },
          settled: true,
        }),
      ),
    );
    expect(presentation).toEqual({
      kind: "unavailable",
      reason: "retention_gap",
      retryable: false,
    });
  });

  it("keeps an explicit waiting state for terminal turns whose brief has not landed", () => {
    const presentation = turnResultPresentation(
      group(
        turnSummary("t", 1, {
          execution: { kind: "terminal", outcome: "completed" },
          result: { kind: "pending" },
        }),
      ),
    );
    expect(presentation.kind).toBe("pending");
  });

  it("propagates no-brief reasons for aborted turns", () => {
    const presentation = turnResultPresentation(
      group(
        turnSummary("t", 1, {
          execution: { kind: "terminal", outcome: "aborted" },
          result: { kind: "none", reason: { kind: "aborted" } },
        }),
      ),
    );
    expect(presentation).toEqual({
      kind: "terminal_without_result",
      outcome: "aborted",
      reason: { kind: "aborted" },
    });
  });
});


describe("turn execution state", () => {
  it.each([
    ["completed", null, "completed"],
    ["aborted", null, "interrupted"],
    ["interrupted", null, "interrupted"],
    ["deferred_to_fallback", null, "recovering"],
    ["provider_failed_needs_recovery", null, "failed"],
    ["baseline_over_budget", null, "failed"],
    ["completed", { kind: "waiting" }, "waiting"],
  ] as const)("distinguishes %s from successful completion", (outcome, attention, expected) => {
    expect(turnExecutionPresentation(group(turnSummary("t", 1, {
      execution: { kind: "terminal", outcome }, attention, settled: true,
      result: { kind: "available" }, brief_ids: ["brief-1"],
    })))).toBe(expected);
  });
  it("does not call a result final until settlement", () => {
    expect(turnExecutionPresentation(group(turnSummary("t", 1, {
      execution: { kind: "terminal", outcome: "completed" },
      result: { kind: "available" }, brief_ids: ["brief-1"], settled: false,
    })))).toBe("waitingResult");
  });
});

describe("local pending operator prompts", () => {
  const emptyBriefState = {
    briefs: new Map(),
    briefLoadStates: new Map(),
    detailLoadStates: new Map(),
  };

  function baseModel() {
    return buildConversationSessionModel({
      status: { kind: "ready" },
      view: stateView([]),
      historyState: { kind: "idle" },
      ...emptyBriefState,
    });
  }

  it("surfaces unconfirmed prompts as a local echo", () => {
    const model = buildConversationSessionModel({
      status: { kind: "ready" },
      view: stateView([]),
      historyState: { kind: "idle" },
      ...emptyBriefState,
      localPendingPrompts: [
        { clientId: "client-1", text: "hello", createdAt: "2026-09-29T00:00:00Z" },
      ],
    });
    expect(model.localPendingPrompts).toHaveLength(1);
    expect(baseModel().localPendingPrompts).toHaveLength(0);
  });

  it("retires a prompt once the server view echoes its message id", () => {
    const echoed = stateView([], {
      pending_inputs: [{
        message_id: "msg-1",
        revision: 1,
        state: "queued",
        preview: "hello",
        presentation_class: "operator",
        created_at: "2026-09-29T00:00:01Z",
        actor_display_name: undefined,
      }],
    });
    const model = buildConversationSessionModel({
      status: { kind: "ready" },
      view: echoed,
      historyState: { kind: "idle" },
      ...emptyBriefState,
      localPendingPrompts: [
        { clientId: "client-1", text: "hello", createdAt: "2026-09-29T00:00:00Z", messageId: "msg-1" },
        { clientId: "client-2", text: "still sending", createdAt: "2026-09-29T00:00:02Z" },
      ],
    });
    expect(model.localPendingPrompts.map((entry) => entry.clientId)).toEqual(["client-2"]);
  });
});
