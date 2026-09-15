import { describe, expect, it } from "vitest";

import type {
  ConversationCheckpoint,
  ConversationHistoryCursor,
  ConversationStateView,
  ConversationTurnSummary,
} from "@holon/conversation-sdk";

import {
  buildConversationSessionModel,
  isBackgroundTurn,
  turnResultPresentation,
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
    expect(model.activeTurn?.turnId).toBe("turn-1");
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

  it("carries pending inputs and history paging state", () => {
    const model = buildConversationSessionModel({
      status: { kind: "ready" },
      view: stateView([], {
        pending_inputs: [
          { message_id: "m-1", revision: 1, state: "queued" },
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
    expect(presentation.kind).toBe("terminal_without_result");
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

describe("isBackgroundTurn", () => {
  it("marks non-operator presentation classes as background", () => {
    expect(isBackgroundTurn(group(turnSummary("t", 1)))).toBe(false);
    expect(
      isBackgroundTurn(
        group(turnSummary("t", 1, { presentation_class: "timer" })),
      ),
    ).toBe(true);
  });
});
