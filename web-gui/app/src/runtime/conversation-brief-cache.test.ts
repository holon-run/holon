import "fake-indexeddb/auto";

import { describe, expect, it } from "vitest";

import type { BriefRecord } from "@holon/conversation-sdk";

import { createConversationBriefCache } from "./conversation-brief-cache";
import {
  BRIEF_CACHE_SCHEMA_VERSION,
  cachePutConversationBrief,
} from "./idb-cache";

function briefRecord(id: string, agentId = "web"): BriefRecord {
  return {
    id,
    agent_id: agentId,
    workspace_id: "ws-a",
    kind: "result",
    created_at: "2026-01-01T00:00:00Z",
    content_source: { kind: "inline" },
    text: `brief ${id}`,
    attachments: null,
    related_message_id: null,
    related_task_id: null,
  };
}

describe("conversation brief cache", () => {
  it("round-trips briefs through IndexedDB per remote+agent scope", async () => {
    const cache = createConversationBriefCache("remote-a", "web");
    await cache.put("brief-1", briefRecord("brief-1"));

    const hit = await cache.get("brief-1");
    expect(hit?.text).toBe("brief brief-1");

    // Other scopes never observe the entry.
    const otherRemote = createConversationBriefCache("remote-b", "web");
    expect(await otherRemote.get("brief-1")).toBeNull();
    const otherAgent = createConversationBriefCache("remote-a", "agent-2");
    expect(await otherAgent.get("brief-1")).toBeNull();
    expect(await cache.get("brief-missing")).toBeNull();
  });

  it("ignores entries written by an incompatible schema version", async () => {
    await cachePutConversationBrief({
      remoteKey: "remote-a",
      agentId: "web",
      briefId: "brief-stale",
      schemaVersion: BRIEF_CACHE_SCHEMA_VERSION - 1,
      brief: briefRecord("brief-stale"),
      cachedAt: 1,
    });
    const cache = createConversationBriefCache("remote-a", "web");
    expect(await cache.get("brief-stale")).toBeNull();
  });

  it("ignores corrupted brief payloads", async () => {
    await cachePutConversationBrief({
      remoteKey: "remote-a",
      agentId: "web",
      briefId: "brief-corrupt",
      schemaVersion: BRIEF_CACHE_SCHEMA_VERSION,
      brief: { id: "brief-corrupt" } as never,
      cachedAt: 1,
    });
    const cache = createConversationBriefCache("remote-a", "web");
    expect(await cache.get("brief-corrupt")).toBeNull();
  });

  it("ignores entries whose id or agent does not match the request", async () => {
    await cachePutConversationBrief({
      remoteKey: "remote-a",
      agentId: "web",
      briefId: "brief-foreign",
      schemaVersion: BRIEF_CACHE_SCHEMA_VERSION,
      brief: briefRecord("brief-foreign", "other-agent"),
      cachedAt: 1,
    });
    const cache = createConversationBriefCache("remote-a", "web");
    expect(await cache.get("brief-foreign")).toBeNull();
  });
});
