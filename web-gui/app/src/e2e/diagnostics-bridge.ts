import { useRuntimeStore } from "../runtime/runtime-store";

export interface HolonE2eSnapshot {
  route: string;
  bootstrapLoading: boolean;
  bootstrapError?: string;
  globalStreamStatus: string;
  discovery: {
    mode: string;
    freshness: string;
  };
  connection: {
    mode: string;
    source: string;
    summary: string;
  };
  agentIds: string[];
}

export interface HolonE2eDiagnostics {
  snapshot(): HolonE2eSnapshot;
  subscribe(listener: (snapshot: HolonE2eSnapshot) => void): () => void;
}

declare global {
  interface Window {
    __HOLON_E2E__?: HolonE2eDiagnostics;
  }
}

function snapshot(): HolonE2eSnapshot {
  const state = useRuntimeStore.getState();
  return {
    route: state.route,
    bootstrapLoading: state.bootstrapLoading,
    bootstrapError: state.bootstrapError,
    globalStreamStatus: state.globalStreamStatus,
    discovery: {
      mode: state.discovery.mode,
      freshness: state.discovery.freshness,
    },
    connection: {
      mode: state.bootstrap.connection.mode,
      source: state.bootstrap.connection.source,
      summary: state.bootstrap.connection.summary,
    },
    agentIds: state.bootstrap.agents.map((agent) => agent.id),
  };
}

window.__HOLON_E2E__ = Object.freeze({
  snapshot,
  subscribe(listener: (value: HolonE2eSnapshot) => void) {
    return useRuntimeStore.subscribe(() => listener(snapshot()));
  },
});
