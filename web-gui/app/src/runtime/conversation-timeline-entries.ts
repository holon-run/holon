import type { ConversationActivity, TurnInputSummary } from "@holon/conversation-sdk";

export type ConversationTimelineEntry =
  | { kind: "input"; id: string; input: TurnInputSummary }
  | { kind: "activity"; id: string; activity: ConversationActivity };

/** Input keys and activity keys share the server's durable sequence, not timestamps. */
export function conversationTimelineEntries(
  inputs: readonly TurnInputSummary[], activities: readonly ConversationActivity[],
): ConversationTimelineEntry[] {
  const entries: ConversationTimelineEntry[] = [
    ...inputs.map((input): ConversationTimelineEntry => ({ kind: "input", id: input.message_id, input })),
    ...activities.filter((activity) => activity.kind !== "operator")
      .map((activity): ConversationTimelineEntry => ({ kind: "activity", id: activity.id, activity })),
  ];
  const key = (entry: ConversationTimelineEntry) => entry.kind === "input" ? entry.input.activity_key : entry.activity.key;
  return entries.sort((left, right) => {
    const a = key(left), b = key(right);
    // Older servers have no interjection key: retain the input, without inventing a timestamp.
    if (!a || !b) return a ? -1 : b ? 1 : 0;
    return a.event_seq - b.event_seq || a.activity_id.localeCompare(b.activity_id);
  });
}
