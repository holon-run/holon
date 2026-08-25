import {
  ledgerReadMarkerDecision,
  useRuntimeStore,
} from "../runtime/runtime-store";
import {
  AGENT_SESSIONS_STORE,
  LEDGER_DB_NAME,
  LEDGER_DB_VERSION,
  PENDING_HYDRATION_STORE,
  READ_STATES_STORE,
} from "../runtime/event-ledger/db";
import type {
  LedgerAgentSessionRecord,
  LedgerHydrationJobRecord,
  LedgerReadStateRecord,
} from "../runtime/event-ledger/ledger";

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
  ledger(agentId: string): Promise<HolonE2eLedgerSnapshot | null>;
  subscribe(listener: (snapshot: HolonE2eSnapshot) => void): () => void;
}

export interface HolonE2eLedgerSnapshot {
  agentId: string;
  ingestedThroughSeq: number;
  projectionReadyThroughSeq: number;
  pendingHydrationJobs: number;
  failedHydrationJobs: number;
  blockedByEventSeq?: number;
  blockedReason?: "pending_hydration";
  readThroughEventSeq?: number;
  unreadCount?: number;
  readGateDecision: {
    mayAdvance: boolean;
    candidateSeq?: number;
    reason?: string;
  };
  readGateContext: {
    route: string;
    selectedAgentId: string;
    documentVisible: boolean;
    discoveryFreshness: string;
    sessionLoading?: boolean;
    sessionSyncStatus?: string;
    sessionLiveStatus?: string;
    sessionGapCount?: number;
    pendingProjectionHydrationCount?: number;
  };
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

function requestResult<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

async function ledgerSnapshot(agentId: string): Promise<HolonE2eLedgerSnapshot | null> {
  const db = await requestResult(indexedDB.open(LEDGER_DB_NAME, LEDGER_DB_VERSION));
  try {
    const transaction = db.transaction(
      [AGENT_SESSIONS_STORE, PENDING_HYDRATION_STORE, READ_STATES_STORE],
      "readonly",
    );
    const sessions = await requestResult(
      transaction.objectStore(AGENT_SESSIONS_STORE).getAll(),
    ) as LedgerAgentSessionRecord[];
    const session = sessions
      .filter((candidate) => candidate.agentId === agentId)
      .sort((left, right) => right.updatedAt - left.updatedAt)[0];
    if (!session) return null;
    const scope = [
      session.remoteKey,
      session.runtimeId,
      session.visibilityScopeId,
      session.eventLogEpoch,
      session.agentId,
    ];
    const jobs = await requestResult(
      transaction.objectStore(PENDING_HYDRATION_STORE).index("byScope").getAll(scope),
    ) as LedgerHydrationJobRecord[];
    const readState = await requestResult(
      transaction.objectStore(READ_STATES_STORE).get(scope),
    ) as LedgerReadStateRecord | undefined;
    const pending = jobs.filter((job) => job.state !== "failed");
    const storeState = useRuntimeStore.getState();
    const runtimeSession = storeState.sessionsByAgentId[agentId];
    return {
      agentId,
      ingestedThroughSeq: session.ingestedThroughSeq ?? 0,
      projectionReadyThroughSeq: session.projectionReadyThroughSeq ?? 0,
      pendingHydrationJobs: pending.length,
      failedHydrationJobs: jobs.length - pending.length,
      blockedByEventSeq: pending.length
        ? Math.min(...pending.map((job) => job.createdByEventSeq))
        : undefined,
      blockedReason: pending.length ? "pending_hydration" : undefined,
      readThroughEventSeq: readState?.readThroughEventSeq,
      unreadCount: storeState.ledgerUnreadByAgentId[agentId]?.count,
      readGateDecision: ledgerReadMarkerDecision(agentId),
      readGateContext: {
        route: storeState.route,
        selectedAgentId: storeState.selectedAgentId,
        documentVisible: document.visibilityState === "visible",
        discoveryFreshness: storeState.discovery.freshness,
        sessionLoading: runtimeSession?.loading,
        sessionSyncStatus: runtimeSession?.syncStatus,
        sessionLiveStatus: runtimeSession?.liveStatus,
        sessionGapCount: runtimeSession?.gaps.length,
        pendingProjectionHydrationCount: runtimeSession
          ? Object.values(runtimeSession.briefHydrationById).filter(
            (hydration) => hydration.status === "loading",
          ).length
          : undefined,
      },
    };
  } finally {
    db.close();
  }
}

window.__HOLON_E2E__ = Object.freeze({
  snapshot,
  ledger: ledgerSnapshot,
  subscribe(listener: (value: HolonE2eSnapshot) => void) {
    return useRuntimeStore.subscribe(() => listener(snapshot()));
  },
});
