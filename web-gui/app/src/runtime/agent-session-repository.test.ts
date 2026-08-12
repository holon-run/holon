import { afterEach, describe, expect, it, vi } from "vitest";

import {
  AgentSessionRepository,
  type AgentSessionRepositoryDependencies,
  type AgentSessionRepositoryState,
} from "./agent-session-repository";
import type { StreamEventEnvelopeDto } from "./client";
import { emptyAgentSession } from "./conversation-store";
import type { AgentSessionState } from "./runtime-store-helpers";
import type { RuntimeMessageEnvelope } from "./types";

interface TestState extends AgentSessionRepositoryState {}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function event(seq: number): StreamEventEnvelopeDto {
  return {
    id: `event-${seq}`,
    agent_id: "agent-a",
    event_seq: seq,
    type: "brief_created",
    payload: {},
  };
}

function createHarness(session: AgentSessionState = emptyAgentSession()) {
  let generation = 1;
  let state: TestState = {
    route: "agent",
    selectedAgentId: "agent-a",
    displayLevel: "info",
    globalStreamStatus: "streaming",
    sessionsByAgentId: { "agent-a": session },
    refreshAgentDetail: vi.fn(async () => undefined),
    refreshAgentWorkItems: vi.fn(async () => undefined),
    refreshAgentState: vi.fn(async () => undefined),
  };
  const client = {
    getAgentEvents: vi.fn(),
    getAgentMessagesBatch: vi.fn(async () => ({
      messages: [] as RuntimeMessageEnvelope[],
      missing_message_ids: [] as string[],
    })),
    getAgentTranscriptEntriesBatch: vi.fn(async () => ({
      entries: [],
      missing_entry_ids: [],
    })),
    getAgentBriefsById: vi.fn(async () => ({ recordsById: {}, notFoundIds: [] })),
  };
  const mergedPages: Array<{
    events: StreamEventEnvelopeDto[];
    options: { newestSeq?: number; historyDisplayLevel?: string };
  }> = [];
  const dependencies: AgentSessionRepositoryDependencies<TestState> = {
    get: () => state,
    set: (update) => {
      const partial = typeof update === "function" ? update(state) : update;
      state = { ...state, ...partial };
    },
    getClient: () => client,
    getConnectionConfig: () => ({ mode: "local" }),
    getGeneration: () => generation,
    isCurrentGeneration: (candidate) => candidate === generation,
    mergeRemoteCache: () => ({ partial: {}, restoredAgentIds: [] }),
    mergeAgentCache: () => ({}),
    markCacheUnavailable: () => ({}),
    mergeEventPage: (
      current,
      agentId,
      events,
      _oldestSeq,
      _hasOlder,
      _displayLevel,
      options = {},
    ) => {
      mergedPages.push({ events, options });
      return {
        sessionsByAgentId: {
          ...current.sessionsByAgentId,
          [agentId]: {
            ...current.sessionsByAgentId[agentId],
            newestSeq: options.newestSeq,
          },
        },
      };
    },
    mergeMessages: (current, agentId, messages) => ({
      sessionsByAgentId: {
        ...current.sessionsByAgentId,
        [agentId]: {
          ...current.sessionsByAgentId[agentId],
          messagesById: Object.fromEntries(messages.map((message) => [message.id, message])),
        },
      },
    }),
    mergeTranscripts: () => ({}),
    mergeBriefs: () => ({}),
    markBriefHydrationStarted: (current, agentId, briefIds) => {
      const currentSession = current.sessionsByAgentId[agentId];
      const briefHydrationById = { ...currentSession.briefHydrationById };
      for (const briefId of briefIds) {
        briefHydrationById[briefId] = {
          briefId,
          status: "loading",
          attempt: (briefHydrationById[briefId]?.attempt ?? 0) + 1,
        };
      }
      return {
        sessionsByAgentId: {
          ...current.sessionsByAgentId,
          [agentId]: { ...currentSession, briefHydrationById },
        },
      };
    },
    markBriefHydrationFailed: (current, agentId, briefIds, errorKind) => {
      const currentSession = current.sessionsByAgentId[agentId];
      const briefHydrationById = { ...currentSession.briefHydrationById };
      for (const briefId of briefIds) {
        briefHydrationById[briefId] = {
          ...briefHydrationById[briefId],
          briefId,
          status: "failed",
          errorKind,
        };
      }
      return {
        sessionsByAgentId: {
          ...current.sessionsByAgentId,
          [agentId]: { ...currentSession, briefHydrationById },
        },
      };
    },
    markHydrationError: () => ({}),
    updateTargetEventState: (current, agentId, update) => ({
      sessionsByAgentId: {
        ...current.sessionsByAgentId,
        [agentId]: {
          ...emptyAgentSession(),
          ...current.sessionsByAgentId[agentId],
          targetEventLoading: update.loading,
          targetEventError: update.error,
        },
      },
    }),
    missingMessageIds: (current) => Object.keys(current?.referencedMessageIds ?? {}),
    missingTranscriptIds: () => [],
    missingBriefIds: (current) => Object.keys(current?.referencedBriefIds ?? {}),
    cachedReadState: () => undefined,
    rebaseRecovery: vi.fn(),
    isWorkItemInvalidationEvent: () => false,
    isAgentStateInvalidationEvent: () => false,
    catchUpErrorKind: () => "request_failed",
  };
  return {
    client,
    dependencies,
    getState: () => state,
    mergedPages,
    repository: new AgentSessionRepository(dependencies),
    advanceGeneration: () => {
      generation += 1;
    },
  };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("AgentSessionRepository", () => {
  it("deduplicates ensure operations and permits a new operation after completion", async () => {
    const harness = createHarness();
    const pending = deferred<void>();
    const operation = vi.fn(() => pending.promise);

    const first = harness.repository.runEnsureOnce("agent-a", operation);
    const second = harness.repository.runEnsureOnce("agent-a", operation);

    expect(second).toBe(first);
    expect(operation).toHaveBeenCalledTimes(1);
    pending.resolve();
    await first;
    await harness.repository.runEnsureOnce("agent-a", operation);
    expect(operation).toHaveBeenCalledTimes(2);
  });

  it("advances gap catch-up with the consumed event cursor", async () => {
    const harness = createHarness({
      ...emptyAgentSession(),
      newestSeq: 3,
      gaps: [{ afterSeq: 3, beforeSeq: 10 }],
    });
    harness.client.getAgentEvents
      .mockResolvedValueOnce({
        events: [event(12), event(10)],
        oldest_seq: 10,
        has_older: true,
      })
      .mockResolvedValueOnce({
        events: [event(12)],
        oldest_seq: 12,
        has_older: true,
        has_newer: false,
      })
      .mockResolvedValueOnce({
        events: [event(4), event(5)],
        has_newer: true,
      })
      .mockResolvedValueOnce({
        events: [event(6), event(10)],
        has_newer: true,
      });

    await harness.repository.catchUpEvents("agent-a", "info");

    expect(harness.client.getAgentEvents.mock.calls).toEqual([
      ["agent-a", { limit: 100, order: "desc" }],
      ["agent-a", { limit: 80, order: "desc", displayLevel: "info" }],
      ["agent-a", { afterSeq: 3, limit: 100, order: "asc" }],
      ["agent-a", { afterSeq: 5, limit: 100, order: "asc" }],
    ]);
    expect(harness.mergedPages.map((page) => page.options.newestSeq)).toEqual([
      12,
      12,
      5,
      10,
    ]);
  });

  it("deduplicates message hydration and cancels stale generation results", async () => {
    const harness = createHarness({
      ...emptyAgentSession(),
      referencedMessageIds: { "message-1": true },
    });
    const pending = deferred<{ messages: Array<{ id: string }>; missing_message_ids: string[] }>();
    harness.client.getAgentMessagesBatch.mockReturnValue(pending.promise);

    harness.repository.hydrateSession("agent-a", "info");
    harness.repository.hydrateSession("agent-a", "info");
    expect(harness.client.getAgentMessagesBatch).toHaveBeenCalledTimes(1);

    harness.advanceGeneration();
    pending.resolve({ messages: [{ id: "message-1" }], missing_message_ids: [] });
    await pending.promise;
    await Promise.resolve();

    expect(harness.getState().sessionsByAgentId["agent-a"].messagesById).toEqual({});
  });

  it("hydrates selected content without requiring the agent route", () => {
    const harness = createHarness({
      ...emptyAgentSession(),
      referencedMessageIds: { "message-1": true },
      referencedBriefIds: { "brief-1": true },
    });
    harness.dependencies.set({ route: "dashboard" });

    harness.repository.hydrateSelectedContent("agent-a", "info");

    expect(harness.client.getAgentMessagesBatch).toHaveBeenCalledWith(
      "agent-a",
      ["message-1"],
    );
    expect(harness.client.getAgentBriefsById).not.toHaveBeenCalled();
  });

  it("hydrates briefs without loading background agent content", () => {
    const harness = createHarness({
      ...emptyAgentSession(),
      referencedMessageIds: { "message-1": true },
      referencedBriefIds: { "brief-1": true },
    });
    harness.dependencies.set({ selectedAgentId: "agent-b" });

    harness.repository.hydrateSelectedContent("agent-a", "info");
    harness.repository.hydrateBriefs("agent-a", "info");

    expect(harness.client.getAgentMessagesBatch).not.toHaveBeenCalled();
    expect(harness.client.getAgentTranscriptEntriesBatch).not.toHaveBeenCalled();
    expect(harness.client.getAgentBriefsById).toHaveBeenCalledWith(
      "agent-a",
      ["brief-1"],
    );
  });

  it("retries failed brief hydration after the configured delay", async () => {
    vi.useFakeTimers();
    const harness = createHarness({
      ...emptyAgentSession(),
      referencedBriefIds: { "brief-1": true },
    });
    harness.client.getAgentBriefsById
      .mockRejectedValueOnce(new Error("temporary failure"))
      .mockResolvedValueOnce({
        recordsById: { "brief-1": { id: "brief-1", text: "Recovered" } },
        notFoundIds: [],
      });

    harness.repository.hydrateSession("agent-a", "info");
    await vi.waitFor(() =>
      expect(harness.client.getAgentBriefsById).toHaveBeenCalledTimes(1),
    );
    await vi.advanceTimersByTimeAsync(1_000);

    expect(harness.client.getAgentBriefsById).toHaveBeenCalledTimes(2);
    expect(
      harness.getState().sessionsByAgentId["agent-a"].briefHydrationById["brief-1"]?.attempt,
    ).toBe(2);
  });
});
