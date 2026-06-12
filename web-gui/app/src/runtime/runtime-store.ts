import { create } from "zustand";

import { createRuntimeClient } from "./client";
import type { AgentDetail, DisplayLevel, RouteKey, RuntimeBootstrap } from "./types";

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
}

const runtimeClient = createRuntimeClient();

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
