import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

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
  it("renders inputs and briefs without visible turn chrome", () => {
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
    expect(html).toContain('aria-label="Conversation turn 2"');
    expect(html).not.toContain("timeline-turn-rail");
    expect(html.indexOf("Completed")).toBeLessThan(html.indexOf("这是结果内容 markdown"));
  });

  it("keeps legacy historical briefs readable without claiming delivery is pending", () => {
    const html = renderTimeline([turnSummary("legacy", 1, {
      execution: { kind: "terminal", outcome: "completed" },
      result: { kind: "available" }, settled: false, brief_ids: ["brief-1"],
    })]);
    expect(html).toContain("这是结果内容 markdown");
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain("waiting for the result");
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
    expect(html).toContain("This turn was stopped");
    expect(html).toContain("Result unavailable");
    expect(html).not.toContain("waiting for the result");
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
  it("preserves readable assistant text without flattening Markdown", () => {
    const long = "x".repeat(400);
    const summary = summarizeActivity({
      kind: "assistant",
      id: "a-1",
      key: { event_seq: 1, activity_id: "a-1" },
      revision: 1,
      summary: JSON.stringify({ blocks: [{ type: "text", text: long }] }),
    });
    expect(summary).toEqual({ display: long, plain: long });
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


describe("conversation presentation boundaries", () => {
  it("preserves Markdown links and ordinary JSON in display text", () => {
    for (const raw of ['[Source](https://example.com)', '{"files": 3}', '[1, 2]']) {
      expect(summarizeActivity({ kind: "assistant", id: "a", key: { event_seq: 1, activity_id: "a" }, revision: 1, summary: raw }))
        .toEqual({ display: raw, plain: raw });
    }
  });

  it("never returns hidden blocks or malformed transcript JSON as display or inspector text", () => {
    for (const raw of [
      '{"blocks":[{"type":"thinking","text":"private reasoning","signature":"secret"}]}',
      '{"blocks":[{"type":"text","text":"truncated',
      '{"active_model":"private","checkpoint":"secret"}',
    ]) {
      expect(summarizeActivity({ kind: "assistant", id: "a", key: { event_seq: 1, activity_id: "a" }, revision: 1, summary: raw }))
        .toEqual({ display: "", plain: "" });
    }
  });
  it("shows an early Brief while execution is still active", () => {
    const html = renderTimeline([turnSummary("active", 1, { brief_ids: ["brief-1"], result: { kind: "available" } })]);
    expect(html).toContain("Working");
    expect(html).toContain("这是结果内容 markdown");
  });
  it("puts queued input after history and exposes system provenance without a user bubble", () => {
    const html = renderTimeline([turnSummary("background", 1, {
      presentation_class: "system", inputs: [{ message_id: "system", preview: "recheck" }],
      execution: { kind: "terminal", outcome: "completed" }, result: { kind: "available" },
      settled: true, brief_ids: ["brief-1"],
    })], { pendingInputs: [{ message_id: "queued", revision: 1, state: "queued", preview: "new prompt" }] } as never);
    expect(html).toContain("System wake");
    expect(html).not.toContain('class="conversation-input-line"');
    expect(html.indexOf("new prompt")).toBeGreaterThan(html.indexOf("这是结果内容 markdown"));
  });
});
