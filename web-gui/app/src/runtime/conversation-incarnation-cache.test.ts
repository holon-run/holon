import "fake-indexeddb/auto";
import { describe, expect, it } from "vitest";
import type { BriefRecord, ConversationSummaryResponse } from "@holon/conversation-sdk";

import { conversationCacheKey } from "./conversation-scope-store";
import { createConversationBriefCache } from "./conversation-brief-cache";
import { createConversationSnapshotCache } from "./conversation-snapshot-cache";

describe("incarnation-scoped persistent conversation caches", () => {
  it("isolates snapshots and briefs from old incarnations, legacy keys, and other logins", async () => {
    const user = { authMethod: "token", userId: "alice" };
    const oldKey = conversationCacheKey("incarnation-cache", user, 1)!;
    const newKey = conversationCacheKey("incarnation-cache", user, 2)!;
    const summary: ConversationSummaryResponse = {
      schema_version: 2, query_version: 2,
      runtime_id: "runtime", event_log_epoch: "epoch", visibility_scope_id: "scope",
      snapshot_through_seq: 1, event_head_seq: 1, oldest_retained_seq: 1,
      snapshot_cursor: "checkpoint" as ConversationSummaryResponse["snapshot_cursor"],
      turns: [], active_turns: [], pending_inputs: [],
      next_before_cursor: null, has_more: false,
    };
    const brief: BriefRecord = {
      id: "brief", agent_id: "child", workspace_id: "workspace", kind: "result",
      created_at: "2026-10-15T16:00:00Z", content_source: { kind: "inline" },
      text: "old incarnation", attachments: null, related_message_id: null, related_task_id: null,
    };
    const oldSnapshot = createConversationSnapshotCache(oldKey, "child");
    const oldBriefs = createConversationBriefCache(oldKey, "child");
    await oldSnapshot.store({ etag: "old", summary });
    await oldBriefs.put("brief", brief);
    for (const key of [
      newKey,
      conversationCacheKey("incarnation-cache", user)!,
      conversationCacheKey("incarnation-cache", { ...user, userId: "bob" }, 1)!,
      conversationCacheKey("incarnation-cache", { ...user, authMethod: "session" }, 1)!,
      conversationCacheKey("other-remote", user, 1)!,
    ]) {
      expect(await createConversationSnapshotCache(key, "child").load()).toBeNull();
      expect(await createConversationBriefCache(key, "child").get("brief")).toBeNull();
    }
    const newSnapshot = createConversationSnapshotCache(newKey, "child");
    const newBriefs = createConversationBriefCache(newKey, "child");
    await newSnapshot.store({ etag: "new", summary });
    await newBriefs.put("brief", { ...brief, text: "new incarnation" });
    await oldSnapshot.clear?.();
    await oldBriefs.clear?.();
    expect((await newSnapshot.load())?.etag).toBe("new");
    expect((await newBriefs.get("brief"))?.text).toBe("new incarnation");
  });
});
