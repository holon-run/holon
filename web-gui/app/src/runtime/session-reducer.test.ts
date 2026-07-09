import { describe, expect, it } from "vitest";

import type { AgentTimelineItem } from "./types";
import type { SessionEventEnvelope } from "./session-reducer";
import {
  compactAgentTimelineItems,
  debugAgentSessionEvents,
  filterTimelineByDisplayLevel,
  mergeAgentTimelineItems,
  reduceAgentSessionTimeline,
} from "./session-reducer";

describe("reduceAgentSessionTimeline", () => {
  it("projects operator input events into the timeline", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          {
            id: "event-1",
            event_seq: 1,
            ts: "2026-06-15T10:00:00Z",
            type: "message_enqueued",
            payload: {
              origin: { kind: "operator" },
              body: { text: "hello" },
            },
          },
        ],
      },
    });

    expect(timeline).toEqual([
      expect.objectContaining({
        id: "event-1",
        kind: "operator",
        label: "Operator input",
        body: "hello",
        minDisplayLevel: "info",
      }),
    ]);
  });

  it("hydrates slim operator message events from the message cache", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          {
            id: "event-1",
            event_seq: 1,
            ts: "2026-06-15T10:00:00Z",
            type: "message_enqueued",
            payload: {
              message_id: "msg-1",
              origin: { kind: "operator" },
            },
          },
        ],
      },
      messagesById: {
        "msg-1": {
          id: "msg-1",
          origin: { kind: "operator" },
          body: { text: "hydrated hello" },
        },
      },
    });

    expect(timeline).toEqual([
      expect.objectContaining({
        id: "event-1",
        kind: "operator",
        body: "hydrated hello",
      }),
    ]);
  });

  it("keeps slim operator message events visible while hydration is pending", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          {
            id: "event-1",
            event_seq: 1,
            ts: "2026-06-15T10:00:00Z",
            type: "message_enqueued",
            payload: {
              message_id: "msg-1",
              origin: { kind: "operator" },
            },
          },
        ],
      },
    });

    expect(timeline).toEqual([
      expect.objectContaining({
        id: "event-1",
        kind: "operator",
        body: "Loading operator input…",
      }),
    ]);
  });

  it("does not dedupe distinct pending slim operator messages by placeholder body", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "event-1",
            event_seq: 1,
            type: "message_enqueued",
            payload: {
              message_id: "msg-1",
              origin: { kind: "operator" },
            },
          }),
          event({
            id: "event-2",
            event_seq: 2,
            type: "message_enqueued",
            payload: {
              message_id: "msg-2",
              origin: { kind: "operator" },
            },
          }),
        ],
      },
    });

    expect(timeline.map((item) => item.id)).toEqual(["event-1", "event-2"]);
  });

  it("keeps the raw event on projected timeline items", () => {
    const rawEvent = event({
      id: "event-raw",
      event_seq: 2,
      type: "brief_created",
      payload: {
        kind: "result",
        text: "done",
      },
    });

    const timeline = reduceAgentSessionTimeline({ events: { events: [rawEvent] } });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        label: "Result",
        body: "done",
        rawEvent,
      }),
    );
  });

  it("hides successful WorkItem mutation tools and keeps failed ones", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          toolEvent("tool-success", "CreateWorkItem"),
          toolEvent("tool-failed", "UpdateWorkItem", { error: "permission denied" }),
        ],
      },
    });

    expect(timeline).toHaveLength(1);
    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "tool-failed",
        kind: "tool",
        label: "Work item update failed",
        body: expect.stringContaining("permission denied"),
      }),
    );
  });

  it("projects ViewImage tools with image context instead of duration-only summaries", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          toolEvent("view-image", "ViewImage", {
            duration_ms: 9700,
            path: "/Users/jolestar/Desktop/Screenshot.png",
            view_image_result: {
              dimensions: { width: 1200, height: 800 },
              visual_observation: "A browser screenshot showing the conversation timeline.",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        kind: "tool",
        label: "Tool finished",
        body: "Viewed image · Screenshot.png · 1200×800 · A browser screenshot showing the conversation timeline. · 9.7s",
      }),
    );
  });

  it("projects ListTasks tools as readable active task summaries", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          toolEvent("list-tasks", "ListTasks", {
            list_tasks_result: {
              total_active: 1,
              returned: 1,
              tasks: [
                {
                  task_id: "task_1",
                  kind: "command_task",
                  status: "running",
                  summary: "Run command: npm run dev",
                },
              ],
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        kind: "tool",
        body: "1 active task · Run command: npm run dev · running · command_task · task_1",
        detail: {
          label: "Tasks",
          text: "Run command: npm run dev · running · command_task · task_1",
          tone: "data",
        },
      }),
    );
  });

  it("projects TaskOutput with task status and output preview", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          toolEvent("task-output", "TaskOutput", {
            task_output_result: {
              retrieval_status: "success",
              task: {
                task_id: "task_abc123",
                status: "completed",
                summary: "Run command: cargo build",
                exit_status: 0,
                output_preview: "Compiling holon v0.1.0\nFinished",
              },
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        kind: "tool",
        body: "Task output · task_abc123 · completed · Run command: cargo build · exit 0",
      }),
    );
    // Duration should be suppressed for read/control tools
    expect(timeline[0].body).not.toContain("ms");
    expect(timeline[0].body).not.toContain("success");
    expect(timeline[0].detail?.tone).toBe("output");
  });

  it("projects TaskOutput retrieval status without collapsing to success or timeout", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          toolEvent("task-output-timeout", "TaskOutput", {
            task_output_result: {
              retrieval_status: "timeout",
              task: {
                task_id: "task_waiting",
                kind: "command_task",
                status: "running",
                summary: "Run command: npm run dev",
                output_preview: "server booting",
              },
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        kind: "tool",
        body: "Task output · task_waiting · retrieval timeout · running · command_task · Run command: npm run dev",
      }),
    );
    expect(timeline[0].body).not.toBe("timeout");
  });

  it("projects TaskOutput with truncated flag", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          toolEvent("task-output-trunc", "TaskOutput", {
            task_output_result: {
              task: { task_id: "task_xyz", status: "running" },
              output_preview: "...partial output...",
              output_truncated: true,
            },
          }),
        ],
      },
    });

    expect(timeline[0].body).toContain("truncated");
  });

  it("projects TaskStatus with status and kind", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          toolEvent("task-status", "TaskStatus", {
            task_status_result: {
              task_id: "task_789",
              status: "running",
              kind: "command_task",
              summary: "Run command: npm test",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        kind: "tool",
        body: "Task status · task_789 · running · command_task · Run command: npm test",
      }),
    );
    expect(timeline[0].body).not.toContain("ms");
  });

  it("projects TaskStop and TaskInput", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          toolEvent("task-stop", "TaskStop", {
            task_stop_result: { task_id: "task_stop1", status: "cancelled" },
          }),
          toolEvent("task-input", "TaskInput", {
            task_input_result: { task_id: "task_in1", status: "accepted" },
            input: "y\n",
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        body: "Stopped task · task_stop1 · cancelled",
      }),
    );
    expect(timeline[1]).toEqual(
      expect.objectContaining({
        body: "Task input · task_in1 · y",
      }),
    );
  });

  it("falls back to the tool name when a tool has no readable summary", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [toolEvent("opaque-tool", "OpaqueTool", { duration_ms: 9700 })],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        body: "OpaqueTool · 9.7s",
      }),
    );
  });

  it("projects structured tool errors as readable error text instead of raw json", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          toolEvent("patch-failed", "ApplyPatch", {
            duration_ms: 120,
            error: {
              message: "context mismatch near projectApplyPatchTool",
              diagnostics: [{ path: "src/runtime/session-reducer.ts" }],
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        kind: "tool",
        label: "Patch failed",
        body: "ApplyPatch · 120ms · context mismatch near projectApplyPatchTool",
        detail: {
          label: "Error",
          text: "context mismatch near projectApplyPatchTool",
          tone: "data",
        },
      }),
    );
  });

  it("parses json-string tool errors before rendering failed tool details", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "patch-failed-json",
            event_seq: 81783,
            type: "tool_execution_failed",
            payload: {
              tool_name: "ApplyPatch",
              error: JSON.stringify({
                kind: "ambiguous_context",
                message: "hunk context matches 8 locations in web-gui/app/src/runtime/session-reducer.ts",
                details: { candidate_count: 8 },
              }),
              error_kind: "ambiguous_context",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        kind: "tool",
        label: "Patch failed",
        body: "ApplyPatch · hunk context matches 8 locations in web-gui/app/src/runtime/session-reducer.ts",
        detail: {
          label: "Error",
          text: "hunk context matches 8 locations in web-gui/app/src/runtime/session-reducer.ts",
          tone: "data",
        },
      }),
    );
  });

  it("renders work_item_picked objective and reason without internal ids", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "picked",
            event_seq: 10,
            type: "work_item_picked",
            payload: {
              reason: "next priority",
              record: {
                id: "work_123",
                objective: "Fix timeline",
              },
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "picked",
        kind: "system",
        label: "Work item",
        body: "Picked work item · Fix timeline · next priority",
        minDisplayLevel: "verbose",
      }),
    );
  });

  it("renders slim work item payloads from preview and top-level fields", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "work-item-updated",
            event_seq: 15,
            type: "work_item_updated",
            payload: {
              work_item_id: "work_123",
              objective_preview: "Improve slim event display",
              plan_status: "ready",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "work-item-updated",
        kind: "system",
        label: "Work item",
        body: "Work Item Updated · Improve slim event display · ready",
        minDisplayLevel: "verbose",
      }),
    );
  });

  it("renders current work item focus from slim top-level ids", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "focus-released",
            event_seq: 16,
            type: "work_item_focus_released",
            payload: {
              current_work_item_id: "work_456",
              reason: "yielded",
              readiness: "runnable",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        body: "Released work item focus · yielded · runnable",
      }),
    );
  });

  it("renders focus release details from top-level work item fields", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "released",
            event_seq: 11,
            type: "work_item_focus_released",
            payload: {
              work_item_id: "work_456",
              reason: "completed",
              readiness: "ready",
              revision: 3,
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "released",
        kind: "system",
        label: "Work item",
        body: "Released work item focus · completed · ready",
        minDisplayLevel: "debug",
      }),
    );
  });

  it("renders completion report promotion details and preview", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "promoted",
            event_seq: 12,
            type: "work_item_completion_report_promoted",
            payload: {
              work_item_id: "work_789",
              brief_id: "brief_123",
              source_turn_index: 7,
              source_round: 2,
              text_preview: "Finished the implementation.",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "promoted",
        kind: "system",
        label: "Work item",
        body: "Promoted completion report · Finished the implementation.",
      }),
    );
  });

  it("renders completion report candidate promotion details and preview", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "candidate-promoted",
            event_seq: 13,
            type: "work_item_completion_report_candidate_promoted",
            payload: {
              work_item_id: "work_abc",
              brief_id: "brief_abc",
              turn_index: 8,
              round: 1,
              text_preview: "Candidate completion text.",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "candidate-promoted",
        kind: "system",
        label: "Work item",
        body: "Promoted completion report candidate · Candidate completion text.",
      }),
    );
  });

  it("projects task lifecycle events with readable status and output previews", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "task-created",
            event_seq: 30,
            type: "task_created",
            payload: {
              task_id: "task_123",
              status: "queued",
              summary: "Run command: npm test",
            },
          }),
          event({
            id: "task-result",
            event_seq: 31,
            type: "task_result_received",
            payload: {
              task_id: "task_123",
              status: "completed",
              summary: "Run command: npm test",
              exit_status: 0,
              output_summary_preview: "42 tests passed",
              output_path: "/tmp/task.log",
            },
          }),
        ],
      },
    });

    // Phase 2: lifecycle events for the same task_id merge into one item
    expect(timeline).toHaveLength(1);
    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "task-result",
        kind: "event",
        label: "Task completed",
        body: "Run command: npm test · exit 0 · 42 tests passed",
        detail: {
          label: "Task details",
          text: "task: task_123 · output: /tmp/task.log · 42 tests passed",
          tone: "output",
        },
      }),
    );
  });

  it("promotes failed task lifecycle events to info", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "task-failed",
            event_seq: 32,
            type: "task_result_received",
            payload: {
              task_id: "task_failed",
              status: "failed",
              summary: "Run command: cargo test",
              exit_status: 101,
              error: "tests failed",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        label: "Task failed",
        body: "Run command: cargo test · exit 101 · tests failed",
        minDisplayLevel: "info",
      }),
    );
  });

  it("merges intermediate task status updates into final lifecycle item", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "t-created",
            event_seq: 40,
            type: "task_created",
            payload: {
              task_id: "task_abc",
              status: "queued",
              summary: "npm run build",
            },
          }),
          event({
            id: "t-running",
            event_seq: 41,
            type: "task_status_updated",
            payload: {
              task_id: "task_abc",
              status: "running",
              summary: "npm run build",
            },
          }),
          event({
            id: "t-done",
            event_seq: 42,
            type: "task_result_received",
            payload: {
              task_id: "task_abc",
              status: "completed",
              summary: "npm run build",
              exit_status: 0,
            },
          }),
        ],
      },
    });

    // Phase 2: all three lifecycle events merge into one item showing final status
    expect(timeline).toHaveLength(1);
    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "t-done",
        kind: "event",
        label: "Task completed",
        body: "npm run build · exit 0",
 sourceIds: expect.arrayContaining(["t-created", "t-running", "t-done"]),
      }),
    );
  });

  it("keeps stale WorkItem reminders at debug level", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "stale",
            event_seq: 11,
            type: "work_item_stale_reminder_injected",
            payload: {
              record: {
                id: "work_123",
                objective: "Fix timeline",
              },
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "stale",
        kind: "system",
        label: "Work item",
        minDisplayLevel: "debug",
      }),
    );
  });

  it("keeps wait condition registrations out of the Info timeline", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "wait-condition",
            event_seq: 14,
            type: "wait_condition_registered",
            payload: {
              reason: "awaiting external change",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "wait-condition",
        kind: "system",
        label: "Waiting",
        minDisplayLevel: "debug",
      }),
    );
    expect(filterTimelineByDisplayLevel(timeline, "info")).toEqual([]);
  });

  it("dedupes assistant previews covered by final briefs", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "preview",
            event_seq: 20,
            type: "assistant_round_recorded",
            payload: {
              text_preview: "Implemented the fix...",
            },
          }),
          event({
            id: "brief",
            event_seq: 21,
            type: "brief_created",
            payload: {
              kind: "result",
              text: "Implemented the fix and verified it.",
            },
          }),
        ],
      },
    });

    expect(timeline).toHaveLength(1);
    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "brief",
        label: "Result",
        body: "Implemented the fix and verified it.",
      }),
    );
  });

  it("hydrates slim brief events from brief records when transcript content is unavailable", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          event({
            id: "brief-event",
            event_seq: 22,
            type: "brief_created",
            payload: {
              id: "brief_123",
              kind: "result",
              content_source: { kind: "inline" },
            },
          }),
        ],
      },
      briefRecordsById: {
        brief_123: {
          id: "brief_123",
          text: "Full persisted brief text.",
          kind: "result",
        },
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        id: "brief-event",
        label: "Result",
        body: "Full persisted brief text.",
      }),
    );
  });
});

describe("filterTimelineByDisplayLevel", () => {
  it("filters items by display level and preserves info-level tool activities", () => {
    const filtered = filterTimelineByDisplayLevel(
      [
        timelineItem({ id: "info", minDisplayLevel: "info" }),
        timelineItem({ id: "verbose", minDisplayLevel: "verbose" }),
        timelineItem({ id: "debug", minDisplayLevel: "debug" }),
        timelineItem({
          id: "activity-parent",
          minDisplayLevel: "debug",
          activities: [
            {
              ...timelineItem({ id: "tool-activity", kind: "tool", minDisplayLevel: "verbose" }),
              meta: "tool_executed · event #1",
            },
            {
              ...timelineItem({ id: "system-activity", kind: "system", minDisplayLevel: "info" }),
              meta: "brief_created · event #2",
            },
          ],
        }),
      ],
      "info",
    );

    expect(filtered.map((item) => item.id)).toEqual(["info", "activity-parent"]);
    expect(filtered[1].activities?.map((activity) => activity.id)).toEqual(["tool-activity"]);
  });

  it("applies explicit item limits after filtering", () => {
    const filtered = filterTimelineByDisplayLevel(
      [
        timelineItem({ id: "first", timestamp: "2026-06-15T10:00:00Z" }),
        timelineItem({ id: "second", timestamp: "2026-06-15T10:01:00Z" }),
        timelineItem({ id: "third", timestamp: "2026-06-15T10:02:00Z" }),
      ],
      "debug",
      { itemLimit: 2 },
    );

    expect(filtered.map((item) => item.id)).toEqual(["second", "third"]);
  });
});

describe("debugAgentSessionEvents", () => {
  it("projects every identifiable raw event as a debug timeline item", () => {
    const events = [
      event({
        id: "assistant-without-preview",
        event_seq: 50,
        type: "assistant_round_recorded",
        payload: {
          turn_id: "turn-1",
        },
      }),
      toolEvent("work-item-mutation", "CreateWorkItem", {
        objective: "Track debug visibility",
      }),
      event({
        type: "no-identity",
        payload: {
          text: "missing id and sequence",
        },
      }),
    ];

    const timeline = debugAgentSessionEvents(events);

    expect(timeline.map((item) => item.id)).toEqual(["debug:assistant-without-preview", "debug:work-item-mutation"]);
    expect(timeline[0]).toEqual(
      expect.objectContaining({
        label: "Assistant Round Recorded",
        meta: "assistant_round_recorded · event #50",
        detail: expect.objectContaining({
          label: "Event details",
          text: "Turn Id: turn-1",
        }),
        rawEvent: events[0],
      }),
    );
    expect(timeline[1]).toEqual(
      expect.objectContaining({
        label: "Tool finished",
        body: expect.stringContaining("Track debug visibility"),
        detail: expect.objectContaining({
          label: "Work item change",
          text: expect.stringContaining("Objective: Track debug visibility"),
        }),
      }),
    );
  });

  it("applies item limits to the newest debug events", () => {
    const timeline = debugAgentSessionEvents(
      [
        event({ id: "old", ts: "2026-06-15T10:00:00Z", type: "brief_created", payload: { text: "old" } }),
        event({ id: "new", ts: "2026-06-15T10:01:00Z", type: "brief_created", payload: { text: "new" } }),
      ],
      { itemLimit: 1 },
    );

    expect(timeline.map((item) => item.id)).toEqual(["debug:new"]);
  });
});

describe("mergeAgentTimelineItems", () => {
  it("merges duplicate assistant items by source identity", () => {
    const merged = mergeAgentTimelineItems(
      [
        timelineItem({
          id: "event-1",
          kind: "assistant",
          body: "same answer",
          meta: "brief_created · event #1",
          sourceIds: ["event-1"],
        }),
      ],
      [
        timelineItem({
          id: "event-1",
          kind: "assistant",
          body: "same answer",
          meta: "brief_created",
          sourceIds: ["brief-1"],
        }),
      ],
    );

    expect(merged).toHaveLength(1);
    expect(merged[0]).toEqual(
      expect.objectContaining({
        id: "event-1",
        sourceIds: ["brief-1", "event-1"],
      }),
    );
  });

  it("keeps repeated final briefs with the same text from different events", () => {
    const merged = mergeAgentTimelineItems(
      [
        timelineItem({
          id: "brief-old",
          kind: "assistant",
          body: "Ignored.",
          meta: "brief_created · event #1",
          sourceIds: ["brief-old"],
        }),
      ],
      [
        timelineItem({
          id: "brief-new",
          kind: "assistant",
          body: "Ignored.",
          meta: "brief_created · event #2",
          sourceIds: ["brief-new"],
        }),
      ],
    );

    expect(merged.map((item) => item.id)).toEqual(["brief-old", "brief-new"]);
  });
});

describe("compactAgentTimelineItems", () => {
  it("flattens meaningful activities and drops ephemeral runtime activities", () => {
    const compacted = compactAgentTimelineItems([
      timelineItem({
        id: "parent",
        activities: [
          {
            ...timelineItem({ id: "ephemeral-tool", kind: "tool" }),
            meta: "tool_executed · event #1",
          },
          {
            ...timelineItem({ id: "work-item", kind: "system", label: "Work item" }),
            meta: "work_item_picked · event #2",
          },
        ],
      }),
    ]);

    expect(compacted.map((item) => item.id)).toEqual(["parent", "work-item"]);
  });
});

function event(overrides: SessionEventEnvelope): SessionEventEnvelope {
  return {
    ts: "2026-06-15T10:00:00Z",
    ...overrides,
  };
}

function toolEvent(id: string, toolName: string, payload: Record<string, unknown> = {}): SessionEventEnvelope {
  return event({
    id,
    event_seq: Number(id.replace(/\D/g, "")) || undefined,
    type: "tool_executed",
    payload: {
      tool_name: toolName,
      duration_ms: 11,
      ...payload,
    },
  });
}

function timelineItem(overrides: Partial<AgentTimelineItem> = {}): AgentTimelineItem {
  return {
    id: "item",
    kind: "event",
    label: "Event",
    body: "body",
    timestamp: "2026-06-15T10:00:00Z",
    meta: "event",
    minDisplayLevel: "info",
    sourceIds: [overrides.id ?? "item"],
    ...overrides,
  };
}

describe("turn_started projection", () => {
  it("projects turn_started events as info-level system items with turn index and trigger", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          {
            id: "evt-turn-1",
            event_seq: 1,
            type: "turn_started",
            ts: "2026-06-15T10:00:00Z",
            payload: {
              turn_index: 42,
              message_kind: "InternalFollowup",
              agent_id: "agent-1",
              message_id: "msg-1",
              run_id: "run-1",
            },
          },
        ],
      },
    });

    expect(timeline).toHaveLength(1);
    const item = timeline[0]!;
    expect(item.kind).toBe("system");
    expect(item.minDisplayLevel).toBe("info");
    expect(item.body).toContain("Turn #42");
    expect(item.body).toContain("internal followup");
    expect(item.meta.startsWith("turn_started")).toBe(true);
  });
});

describe("WebSearch tool projection", () => {
  it("projects WebSearch tool with query and result count on timeline", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          toolEvent("web-search-1", "WebSearch", {
            duration_ms: 3200,
            input: {
              query: "rust async runtime",
              max_results: 5,
            },
            output: {
              query: "rust async runtime",
              provider: "brave",
              mode: "single",
              results: [
                { title: "Tokio", url: "https://tokio.rs", snippet: "Async runtime", source: "tokio.rs" },
                { title: "async-std", url: "https://async.rs", snippet: "Fast runtime", source: "async.rs" },
              ],
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        kind: "tool",
        label: "Web search completed",
        body: expect.stringContaining("Web search · rust async runtime · 2 results"),
      }),
    );
    expect(timeline[0].detail?.label).toBe("Search results");
    expect(timeline[0].detail?.text).toContain("1. Tokio");
    expect(timeline[0].detail?.text).toContain("https://tokio.rs");
    expect(timeline[0].detail?.text).toContain("2. async-std");
  });
});

describe("WebFetch tool projection", () => {
  it("projects WebFetch tool with URL and content on timeline", () => {
    const timeline = reduceAgentSessionTimeline({
      events: {
        events: [
          toolEvent("web-fetch-1", "WebFetch", {
            duration_ms: 5400,
            input: { url: "https://example.com/article", max_chars: 10000 },
            output: {
              url: "https://example.com/article",
              final_url: "https://example.com/article",
              status: 200,
              content_type: "text/html",
              bytes_read: 1024,
              truncated: false,
              text: "# Example Article\n\nContent here.",
            },
          }),
        ],
      },
    });

    expect(timeline[0]).toEqual(
      expect.objectContaining({
        kind: "tool",
        label: "Web fetch completed",
        body: expect.stringContaining("Web fetch · https://example.com/article · 200"),
      }),
    );
    expect(timeline[0].detail?.label).toBe("Fetched content");
    expect(timeline[0].detail?.text).toContain("# Example Article");
  });
});
