import "fake-indexeddb/auto";

import { describe, expect, it } from "vitest";

import type {
  ConversationCheckpoint,
  ConversationSummaryResponse,
} from "@holon/conversation-sdk";

import { createConversationSnapshotCache } from "./conversation-snapshot-cache";
import {
  SNAPSHOT_CACHE_SCHEMA_VERSION,
  cachePutConversationSnapshot,
} from "./idb-cache";

function summarySnapshot(throughSeq = 10): ConversationSummaryResponse {
  return {
    schema_version: 1,
    query_version: 1,
    runtime_id: "runtime-a",
    event_log_epoch: "epoch-a",
    visibility_scope_id: "scope-a",
    snapshot_through_seq: throughSeq,
    event_head_seq: throughSeq,
    oldest_retained_seq: 1,
    snapshot_cursor: `checkpoint-${throughSeq}` as ConversationCheckpoint,
    turns: [],
    active_turns: [],
    pending_inputs: [],
    next_before_cursor: null,
    has_more: false,
  };
}

describe("conversation snapshot cache", () => {
  it("round-trips the latest snapshot with its etag per scope", async () => {
    const cache = createConversationSnapshotCache("remote-a", "web");
    await cache.store({ etag: "etag-10", summary: summarySnapshot(10) });
    await cache.store({ etag: "etag-12", summary: summarySnapshot(12) });

    const entry = await cache.load();
    expect(entry?.etag).toBe("etag-12");
    expect(entry?.summary.snapshot_through_seq).toBe(12);

    const otherRemote = createConversationSnapshotCache("remote-b", "web");
    expect(await otherRemote.load()).toBeNull();
    const otherAgent = createConversationSnapshotCache("remote-a", "agent-2");
    expect(await otherAgent.load()).toBeNull();
  });

  it("ignores entries written by an incompatible schema version", async () => {
    await cachePutConversationSnapshot({
      remoteKey: "remote-a",
      agentId: "web",
      schemaVersion: SNAPSHOT_CACHE_SCHEMA_VERSION - 1,
      etag: null,
      summary: summarySnapshot(),
      cachedAt: 1,
    });
    const cache = createConversationSnapshotCache("remote-a", "web");
    expect(await cache.load()).toBeNull();
  });

  it("ignores corrupted snapshot payloads", async () => {
    await cachePutConversationSnapshot({
      remoteKey: "remote-a",
      agentId: "web",
      schemaVersion: SNAPSHOT_CACHE_SCHEMA_VERSION,
      etag: "etag-x",
      summary: { snapshot_through_seq: "not-a-number" },
      cachedAt: 1,
    });
    const cache = createConversationSnapshotCache("remote-a", "web");
    expect(await cache.load()).toBeNull();
  });
});
