import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import type {
  BriefRecord,
  ConversationBriefLoadState,
  ConversationDetailLoadState,
  ConversationStateView,
  ConversationTurnSummary,
} from "@holon/conversation-sdk";

import "../../i18n";
import { ConversationTimeline, parseInputPreview, summarizeActivity } from "./ConversationTimeline";
import { buildConversationSessionModel } from "../../runtime/conversation-view-model";

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

function stateView(turns: ConversationTurnSummary[]): ConversationStateView {
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
    checkpoint: "checkpoint" as never,
    through_seq: 10,
    reset_reason: null,
  };
}

const brief: BriefRecord = {
  id: "brief-1",
  agent_id: "web",
  workspace_id: "ws",
  kind: "result",
  created_at: "2026-09-15T08:00:00Z",
  content_source: { kind: "inline" },
  text: "这是结果内容 markdown",
  attachments: null,
  related_message_id: null,
  related_task_id: null,
};

function renderTimeline(
  turns: ConversationTurnSummary[],
  options: {
    status?: never;
    pendingInputs?: never;
    onLoadBrief?: (briefId: string) => void;
  } = {},
): string {
  const model = buildConversationSessionModel({
    status: options.status ?? { kind: "ready" },
    view: stateView(turns),
    historyState: { kind: "idle" },
    briefs: new Map([["brief-1", brief]]),
    briefLoadStates: new Map(),
    detailLoadStates: new Map(),
  });
  const withPending =
    options.pendingInputs === undefined
      ? model
      : { ...model, pendingInputs: options.pendingInputs };
  return renderToStaticMarkup(
    <ConversationTimeline
      model={withPending}
      onLoadBrief={options.onLoadBrief ?? (() => {})}
      onLoadDetail={() => {}}
      onLoadOlderActivities={() => {}}
      onRetry={() => {}}
      briefRecord={(id) => (id === "brief-1" ? brief : null)}
      briefLoadState={(id) =>
        id === "brief-1" ? ({ kind: "ready", brief } as ConversationBriefLoadState) : null
      }
      detailLoadState={(): ConversationDetailLoadState => ({ kind: "idle" })}
    />,
  );
}

describe("ConversationTimeline", () => {
  it("renders turn inputs, briefs, and index", () => {
    const html = renderTimeline([
      turnSummary("turn-1", 1, {
        inputs: [{ message_id: "m-1", preview: '{"type":"text","text":"帮我检查发布"}' }],
      }),
      turnSummary("turn-2", 2, {
        execution: { kind: "terminal", outcome: "completed" },
        result: { kind: "available" },
        settled: true,
        brief_ids: ["brief-1"],
      }),
    ]);
    expect(html).toContain("帮我检查发布");
    expect(html).toContain("这是结果内容 markdown");
    expect(html).toContain("#2");
  });

  it("shows terminal-without-result notice and unavailable reason", () => {
    const html = renderTimeline([
      turnSummary("turn-a", 1, {
        execution: { kind: "terminal", outcome: "aborted" },
        result: { kind: "none", reason: { kind: "aborted" } },
      }),
      turnSummary("turn-b", 2, {
        execution: { kind: "terminal", outcome: "completed" },
        result: { kind: "unavailable", reason: "retention_gap", retryable: false },
      }),
    ]);
    expect(html).toContain("with no result brief");
    expect(html).toContain("retention_gap");
  });

  it("renders pending input chips and reconnecting banner", () => {
    const html = renderTimeline([], {
      status: { kind: "reconnecting", attempt: 2, delayMs: 1000 },
      pendingInputs: [{ message_id: "m-2", revision: 1, state: "queued", preview: "" }],
    } as never);
    expect(html).toContain("Queued, waiting to run");
    expect(html).toContain("reconnecting");
  });

  it("echoes pending input preview text on the chip", () => {
    const html = renderTimeline([], {
      pendingInputs: [
        {
          message_id: "m-live",
          revision: 2,
          state: "assigning",
          preview: '{"type":"text","text":"看看这轮渲染"}',
        },
        { message_id: "m-legacy", revision: 1, state: "queued", preview: "" },
      ],
    } as never);
    expect(html).toContain("看看这轮渲染");
    expect(html).toContain("Queued, waiting to run");
  });

  it("renders an empty state when ready with no turns", () => {
    const html = renderTimeline([]);
    expect(html).toContain("No conversation activity yet");
  });

  it("keeps terminal turns collapsed and active turns expanded by default", () => {
    const html = renderTimeline([
      turnSummary("turn-active", 1),
      turnSummary("turn-done", 2, {
        execution: { kind: "terminal", outcome: "completed" },
        result: { kind: "available" },
        brief_ids: ["brief-1"],
      }),
    ]);
    const expanded = renderToStaticMarkup(
      <ConversationTimeline
        model={buildConversationSessionModel({
          status: { kind: "ready" },
          view: stateView([turnSummary("turn-active", 1)]),
          historyState: { kind: "idle" },
          briefs: new Map(),
          briefLoadStates: new Map(),
          detailLoadStates: new Map(),
        })}
        onLoadBrief={() => {}}
        onLoadDetail={() => {}}
        onLoadOlderActivities={() => {}}
        onRetry={() => {}}
        briefRecord={() => null}
        briefLoadState={() => null}
        detailLoadState={() => ({ kind: "idle" })}
      />,
    );
    expect(html).toContain('aria-expanded="false"');
    expect(expanded).toContain('aria-expanded="true"');
  });
});

describe("parseInputPreview", () => {
  it("extracts text from serialized message bodies", () => {
    expect(parseInputPreview('{"type":"text","text":"hello"}')).toBe("hello");
    expect(parseInputPreview('{"type":"json","value":{"a":1}}')).toContain('"a"');
    expect(parseInputPreview("plain")).toBe("plain");
    expect(parseInputPreview("")).toBe("");
  });
});

describe("summarizeActivity", () => {
  it("extracts assistant text blocks and truncates long output", () => {
    const long = "x".repeat(400);
    const summary = summarizeActivity({
      kind: "assistant",
      id: "a-1",
      key: { event_seq: 1, activity_id: "a-1" },
      revision: 1,
      summary: JSON.stringify({ blocks: [{ type: "text", text: long }] }),
    });
    expect(summary.display.endsWith("…")).toBe(true);
    expect(summary.display.length).toBeLessThanOrEqual(241);
  });

  it("surfaces tool summaries verbatim", () => {
    const summary = summarizeActivity({
      kind: "tool",
      id: "t-1",
      key: { event_seq: 2, activity_id: "t-1" },
      revision: 1,
      summary: "completed",
    });
    expect(summary.display).toBe("completed");
  });
});
