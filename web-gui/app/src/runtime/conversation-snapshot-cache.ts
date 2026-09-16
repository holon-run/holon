import {
  decodeConversationSummaryResponse,
} from "@holon/conversation-sdk";

import type {
  ConversationSnapshotCache,
  ConversationSnapshotCacheEntry,
} from "@holon/conversation-sdk";

import {
  cacheClearConversationScope,
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
  let revoked = false;
  return {
    async load() {
      if (revoked) return null;
      const entry = await cacheGetConversationSnapshot(remoteKey, agentId);
      if (
        entry === undefined ||
        entry.schemaVersion !== SNAPSHOT_CACHE_SCHEMA_VERSION
      ) {
        return null;
      }
      try {
        return { etag: entry.etag, summary: decodeConversationSummaryResponse(entry.summary) };
      } catch {
        return null;
      }
    },
    async store(input: ConversationSnapshotCacheEntry) {
      if (revoked) return;
      await cachePutConversationSnapshot({
        remoteKey,
        agentId,
        schemaVersion: SNAPSHOT_CACHE_SCHEMA_VERSION,
        etag: input.etag,
        summary: input.summary,
        cachedAt: Date.now(),
      });
    },
    async clear() {
      revoked = true;
      await cacheClearConversationScope(remoteKey, agentId);
    },
  };
}
