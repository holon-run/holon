import type {
  BriefRecord,
  ConversationBriefCache,
} from "@holon/conversation-sdk";

import {
  cacheClearConversationScope,
  BRIEF_CACHE_SCHEMA_VERSION,
  cacheGetConversationBrief,
  cachePutConversationBrief,
} from "./idb-cache";

/**
 * IndexedDB-backed brief cache for one remote+agent scope. Briefs are
 * immutable final artifacts, reused until expiry or access revocation.
 * Schema and shape/id validation reject corrupted or foreign entries.
 */
export function createConversationBriefCache(
  remoteKey: string,
  agentId: string,
): ConversationBriefCache {
  let revoked = false;
  return {
    async get(briefId) {
      if (revoked) return null;
      const entry = await cacheGetConversationBrief(
        remoteKey,
        agentId,
        briefId,
      );
      if (
        entry === undefined ||
        entry.schemaVersion !== BRIEF_CACHE_SCHEMA_VERSION
      ) {
        return null;
      }
      if (!isBriefRecordLike(entry.brief)) {
        return null;
      }
      const brief = entry.brief;
      return brief.id === briefId && brief.agent_id === agentId
        ? brief
        : null;
    },
    async put(briefId, brief) {
      if (revoked) return;
      await cachePutConversationBrief({
        remoteKey,
        agentId,
        briefId,
        schemaVersion: BRIEF_CACHE_SCHEMA_VERSION,
        brief,
        cachedAt: Date.now(),
      });
    },
    async clear() {
      revoked = true;
      await cacheClearConversationScope(remoteKey, agentId);
    },
  };
}

function isBriefRecordLike(value: unknown): value is BriefRecord {
  if (typeof value !== "object" || value === null) return false;
  const record = value as Record<string, unknown>;
  return (
    typeof record.id === "string" &&
    typeof record.agent_id === "string" &&
    typeof record.workspace_id === "string" &&
    (record.kind === "ack" ||
      record.kind === "result" ||
      record.kind === "failure") &&
    typeof record.created_at === "string" &&
    typeof record.text === "string" &&
    (record.attachments === null || Array.isArray(record.attachments)) &&
    (record.related_message_id === null ||
      typeof record.related_message_id === "string") &&
    (record.related_task_id === null ||
      typeof record.related_task_id === "string")
  );
}
