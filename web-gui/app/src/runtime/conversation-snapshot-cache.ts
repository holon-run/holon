import type {
  ConversationSnapshotCache,
  ConversationSnapshotCacheEntry,
  ConversationSummaryResponse,
} from "@holon/conversation-sdk";

import {
  SNAPSHOT_CACHE_SCHEMA_VERSION,
  cacheGetConversationSnapshot,
  cachePutConversationSnapshot,
} from "./idb-cache";

/**
 * IndexedDB-backed stale-while-revalidate cache of the most recent bootstrap
 * snapshot for one remote+agent scope. A cached snapshot renders instantly
 * on open while the controller revalidates against the server (fresh
 * snapshot, or a 304 confirmed by the cached ETag).
 */
export function createConversationSnapshotCache(
  remoteKey: string,
  agentId: string,
): ConversationSnapshotCache {
  return {
    async load() {
      const entry = await cacheGetConversationSnapshot(remoteKey, agentId);
      if (
        entry === undefined ||
        entry.schemaVersion !== SNAPSHOT_CACHE_SCHEMA_VERSION
      ) {
        return null;
      }
      if (!isSummarySnapshotLike(entry.summary)) {
        return null;
      }
      return { etag: entry.etag, summary: entry.summary };
    },
    async store(input: ConversationSnapshotCacheEntry) {
      await cachePutConversationSnapshot({
        remoteKey,
        agentId,
        schemaVersion: SNAPSHOT_CACHE_SCHEMA_VERSION,
        etag: input.etag,
        summary: input.summary,
        cachedAt: Date.now(),
      });
    },
  };
}

// Structural guard for snapshots restored from untyped storage. The stored
// payload was produced by the SDK decoder before persisting; this check
// only rejects corrupted or foreign records.
function isSummarySnapshotLike(
  value: unknown,
): value is ConversationSummaryResponse {
  if (typeof value !== "object" || value === null) return false;
  const record = value as Record<string, unknown>;
  return (
    typeof record.schema_version === "number" &&
    typeof record.query_version === "number" &&
    typeof record.runtime_id === "string" &&
    typeof record.event_log_epoch === "string" &&
    typeof record.visibility_scope_id === "string" &&
    typeof record.snapshot_through_seq === "number" &&
    typeof record.event_head_seq === "number" &&
    typeof record.oldest_retained_seq === "number" &&
    typeof record.snapshot_cursor === "string" &&
    record.snapshot_cursor.length > 0 &&
    Array.isArray(record.turns) &&
    Array.isArray(record.active_turns) &&
    Array.isArray(record.pending_inputs) &&
    typeof record.has_more === "boolean" &&
    (record.next_before_cursor === null ||
      typeof record.next_before_cursor === "string")
  );
}
