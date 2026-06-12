import { create } from "zustand";

import { createRuntimeClient, type AgentEventStreamSubscription, type StreamEventEnvelopeDto } from "./client";
import { reduceAgentSessionTimeline } from "./session-reducer";
import type { AgentDetail, AgentTimelineItem, DisplayLevel, RouteKey, RuntimeBootstrap } from "./types";

export type AgentLiveStatus = "idle" | "connecting" | "streaming" | "reconnecting" | "error";

export interface AgentSessionState {
  loading: boolean;
  liveStatus: AgentLiveStatus;
  detail: AgentDetail | null;
  eventsBySeq: Record<number, unknown>;
  eventSeqs: number[];
  newestSeq?: number;
  oldestSeq?: number;
  hasOlder?: boolean;
  error?: string;
}

export interface RuntimeStoreState {
  route: RouteKey;
  selectedAgentId: string;
  displayLevel: DisplayLevel;
  inspectorOpen: boolean;
  navCollapsed: boolean;

  bootstrap: RuntimeBootstrap;
  bootstrapLoading: boolean;
  bootstrapError?: string;
  sessionsByAgentId: Record<string, AgentSessionState>;

  setRoute: (route: RouteKey) => void;
  openAgent: (agentId: string) => void;
  setDisplayLevel: (displayLevel: DisplayLevel) => void;
  setInspectorOpen: (open: boolean) => void;
  toggleInspector: () => void;
  toggleNavCollapsed: () => void;
  refreshBootstrap: () => Promise<void>;
  refreshAgentDetail: (agentId: string | undefined, displayLevel: DisplayLevel) => Promise<void>;
  startAgentEventStream: (agentId: string | undefined, displayLevel: DisplayLevel) => void;
  stopAgentEventStream: (agentId: string | undefined) => void;
}

const runtimeClient = createRuntimeClient();
const activeEventStreams = new Map<string, AgentEventStreamSubscription>();
const reconnectTimers = new Map<string, number>();

const emptyBootstrap: RuntimeBootstrap = {
  attentionCount: 0,
  connection: {
    mode: "local",
    source: "fixture",
    summary: "loading runtime dashboard",
  },
  metrics: [],
  agents: [],
};

export const useRuntimeStore = create<RuntimeStoreState>((set, get) => ({
  route: "dashboard",
  selectedAgentId: "",
  displayLevel: "info",
  inspectorOpen: false,
  navCollapsed: false,

  bootstrap: emptyBootstrap,
  bootstrapLoading: true,
  sessionsByAgentId: {},

  setRoute: (route) => set({ route }),
  openAgent: (agentId) => set({ selectedAgentId: agentId, route: "agent" }),
  setDisplayLevel: (displayLevel) => set({ displayLevel }),
  setInspectorOpen: (open) => set({ inspectorOpen: open }),
  toggleInspector: () => set((state) => ({ inspectorOpen: !state.inspectorOpen })),
  toggleNavCollapsed: () => set((state) => ({ navCollapsed: !state.navCollapsed })),

  refreshBootstrap: async () => {
    set({ bootstrapLoading: true, bootstrapError: undefined });
    try {
      const bootstrap = await runtimeClient.getBootstrap();
      set({
        bootstrap,
        bootstrapLoading: false,
        bootstrapError: bootstrap.connection.error,
      });
    } catch (error) {
      set({
        bootstrapLoading: false,
        bootstrapError: error instanceof Error ? error.message : String(error),
      });
    }
  },

  refreshAgentDetail: async (agentId, displayLevel) => {
    if (!agentId) {
      return;
    }

    set((state) => ({
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: {
          ...emptyAgentSession(),
          ...state.sessionsByAgentId[agentId],
          loading: true,
          error: undefined,
        },
      },
    }));

    try {
      const detail = await runtimeClient.getAgentDetail(agentId, displayLevel);
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...emptyAgentSession(),
            ...state.sessionsByAgentId[agentId],
            detail,
            loading: false,
            liveStatus: detail.error ? "error" : "idle",
            eventsBySeq: eventsBySeq(detail.events ?? []),
            eventSeqs: eventSeqs(detail.events ?? []),
            newestSeq: detail.eventCursorSeq ?? detail.newestEventSeq,
            oldestSeq: detail.oldestEventSeq,
            hasOlder: detail.hasOlderEvents,
            error: detail.error,
          },
        },
      }));
    } catch (error) {
      set((state) => ({
        sessionsByAgentId: {
          ...state.sessionsByAgentId,
          [agentId]: {
            ...emptyAgentSession(),
            ...state.sessionsByAgentId[agentId],
            loading: false,
            liveStatus: "error",
            error: error instanceof Error ? error.message : String(error),
          },
        },
      }));
    }
  },

  startAgentEventStream: (agentId, displayLevel) => {
    if (!agentId) return;
    stopAgentEventStream(agentId);
    const session = get().sessionsByAgentId[agentId] ?? emptyAgentSession();
    if (session.detail?.error) return;

    setAgentLiveStatus(set, agentId, "connecting");
    const subscription = runtimeClient.streamAgentEvents(agentId, {
      afterSeq: session.newestSeq,
      limit: 100,
      onOpen: () => setAgentLiveStatus(set, agentId, "streaming"),
      onEvent: (event) => applyStreamEvent(set, agentId, event),
      onError: (error) => {
        activeEventStreams.delete(agentId);
        set((state) => ({
          sessionsByAgentId: {
            ...state.sessionsByAgentId,
            [agentId]: {
              ...emptyAgentSession(),
              ...state.sessionsByAgentId[agentId],
              liveStatus: "reconnecting",
              error: error.message,
            },
          },
        }));
        const timer = window.setTimeout(() => {
          reconnectTimers.delete(agentId);
          get().startAgentEventStream(agentId, displayLevel);
        }, 1500);
        reconnectTimers.set(agentId, timer);
      },
    });
    if (!subscription) {
      setAgentLiveStatus(set, agentId, "idle");
      return;
    }
    activeEventStreams.set(agentId, subscription);
  },

  stopAgentEventStream: (agentId) => {
    if (!agentId) return;
    stopAgentEventStream(agentId);
  },
}));

function emptyAgentSession(): AgentSessionState {
  return {
    loading: false,
    liveStatus: "idle",
    detail: null,
    eventsBySeq: {},
    eventSeqs: [],
  };
}

type StoreSet = (
  partial:
    | Partial<RuntimeStoreState>
    | RuntimeStoreState
    | ((state: RuntimeStoreState) => Partial<RuntimeStoreState> | RuntimeStoreState),
  replace?: false,
) => void;

function stopAgentEventStream(agentId: string): void {
  activeEventStreams.get(agentId)?.close();
  activeEventStreams.delete(agentId);
  const timer = reconnectTimers.get(agentId);
  if (timer != null) {
    window.clearTimeout(timer);
    reconnectTimers.delete(agentId);
  }
}

function setAgentLiveStatus(set: StoreSet, agentId: string, liveStatus: AgentLiveStatus): void {
  set((state) => ({
    sessionsByAgentId: {
      ...state.sessionsByAgentId,
      [agentId]: {
        ...emptyAgentSession(),
        ...state.sessionsByAgentId[agentId],
        liveStatus,
      },
    },
  }));
}

function applyStreamEvent(set: StoreSet, agentId: string, event: StreamEventEnvelopeDto): void {
  const seq = event.event_seq;
  if (seq == null) return;

  set((state) => {
    const current = state.sessionsByAgentId[agentId] ?? emptyAgentSession();
    const eventsBySeq = {
      ...current.eventsBySeq,
      [seq]: event,
    };
    const eventSeqs = Array.from(new Set([...current.eventSeqs, seq])).sort((left, right) => left - right).slice(-300);
    const events = eventSeqs.map((eventSeq) => eventsBySeq[eventSeq]).filter(isStreamEventEnvelope);
    const liveTimeline = reduceAgentSessionTimeline({
      transcript: [],
      briefs: [],
      events: { events },
      eventDisplayLevel: "debug",
    });
    const detail = current.detail
      ? {
          ...current.detail,
          timeline: mergeTimeline(current.detail.timeline, liveTimeline),
          events,
          newestEventSeq: Math.max(seq, current.detail.newestEventSeq ?? 0),
          oldestEventSeq: current.detail.oldestEventSeq ?? eventSeqs[0],
        }
      : current.detail;

    return {
      sessionsByAgentId: {
        ...state.sessionsByAgentId,
        [agentId]: {
          ...current,
          detail,
          eventsBySeq,
          eventSeqs,
          newestSeq: Math.max(seq, current.newestSeq ?? 0),
          oldestSeq: current.oldestSeq ?? eventSeqs[0],
          liveStatus: "streaming",
          error: undefined,
        },
      },
    };
  });
}

function eventsBySeq(events: StreamEventEnvelopeDto[]): Record<number, unknown> {
  return Object.fromEntries(events.filter((event) => event.event_seq != null).map((event) => [event.event_seq, event]));
}

function eventSeqs(events: StreamEventEnvelopeDto[]): number[] {
  return events
    .map((event) => event.event_seq)
    .filter((seq): seq is number => seq != null)
    .sort((left, right) => left - right);
}

function isStreamEventEnvelope(event: unknown): event is StreamEventEnvelopeDto {
  return typeof event === "object" && event !== null;
}

function mergeTimeline(existing: AgentTimelineItem[], incoming: AgentTimelineItem[]): AgentTimelineItem[] {
  const byId = new Map<string, AgentTimelineItem>();
  for (const item of existing) byId.set(item.id, item);
  for (const item of incoming) byId.set(item.id, item);
  return Array.from(byId.values()).sort((left, right) => sortableTime(left.timestamp) - sortableTime(right.timestamp));
}

function sortableTime(value: string): number {
  const timestamp = Date.parse(value);
  return Number.isNaN(timestamp) ? 0 : timestamp;
}
