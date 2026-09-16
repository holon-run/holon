import { expect, it } from "vitest";
import type { ConversationActivity, TurnInputSummary } from "@holon/conversation-sdk";
import { conversationTimelineEntries } from "./conversation-timeline-entries";

it("interleaves inputs using durable activity keys and never repeats operator activities", () => {
  const inputs: TurnInputSummary[] = [2, 4].map((seq) => ({ message_id: `m${seq}`, preview: `Input ${seq}`,
    interjected: true, presentation_class: "operator", activity_key: { event_seq: seq, activity_id: `operator:m${seq}` } }));
  const activities: ConversationActivity[] = [1, 2, 3, 4, 5].map((seq) => ({ kind: seq % 2 ? "assistant" : "operator",
    id: seq % 2 ? `a${seq}` : `operator:m${seq}`, key: { event_seq: seq, activity_id: `a${seq}` }, revision: 1, summary: "Progress" }));
  expect(conversationTimelineEntries(inputs, activities).map((entry) => entry.id)).toEqual(["a1", "m2", "a3", "m4", "a5"]);
  expect(conversationTimelineEntries(inputs, []).map((entry) => entry.id)).toEqual(["m2", "m4"]);
  // A partial detail page must not hide earlier inputs or need a second history fetch.
  expect(conversationTimelineEntries(inputs, activities.slice(-1)).map((entry) => entry.id)).toEqual(["m2", "m4", "a5"]);
});

it("keeps an input when its ordering key is unavailable", () => {
  expect(conversationTimelineEntries([{ message_id: "old", preview: "Still visible" }], [])).toHaveLength(1);
});
