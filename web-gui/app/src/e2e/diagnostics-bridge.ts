import {
  getRuntimeConnectionConfig,
  ledgerStatusForDiagnostics,
  useRuntimeStore,
} from "../runtime/runtime-store";
import {
  resolveConversationScopeKey,
  peekConversationScope,
} from "../runtime/conversation-scope-store";
import { currentRemoteKey } from "../runtime/session-cache";
import {
  AGENT_SESSIONS_STORE,
  LEDGER_DB_NAME,
  LEDGER_DB_VERSION,
  PENDING_HYDRATION_STORE,
  RAW_EVENTS_STORE,
} from "../runtime/event-ledger/db";
import type {
  LedgerAgentSessionRecord,
  LedgerHydrationJobRecord,
  LedgerRawEventRecord,
} from "../runtime/event-ledger/ledger";

export interface HolonE2eSnapshot {
  conversationStatus?: {
    kind: string;
    error?: string;
    hasView: boolean;
  };
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
  ledgerStatus(agentId: string): HolonE2eLedgerStatus | null;
  ledger(agentId: string): Promise<HolonE2eLedgerSnapshot | null>;
  ledgerPartitions(agentId: string): Promise<HolonE2eLedgerPartition[]>;
  subscribe(listener: (snapshot: HolonE2eSnapshot) => void): () => void;
}

export interface HolonE2eLedgerStatus {
  durability: string;
  ingestionState: string;
  ingestionError?: string;
}

export interface HolonE2eLedgerPartition {
  runtimeId: string;
  visibilityScopeId: string;
  eventLogEpoch: string;
  eventSeqs: number[];
  observedHeadSeq?: number;
  readThroughEventSeq?: number;
  certainty?: "exact" | "truncated";
}

export interface HolonE2eLedgerSnapshot {
  agentId: string;
  runtimeId: string;
  visibilityScopeId: string;
  eventLogEpoch: string;
  durability: string;
  ingestionState: string;
  ingestionError?: string;
  ingestedThroughSeq: number;
  observedHeadSeq: number;
  projectionReadyThroughSeq: number;
  pendingHydrationJobs: number;
  failedHydrationJobs: number;
  blockedByEventSeq?: number;
  blockedReason?: "pending_hydration";
  readThroughEventSeq?: number;
  certainty?: "exact" | "truncated";
  historyTruncatedBeforeSeq?: number;
  unreadCount?: number;
}

declare global {
  interface Window {
    __HOLON_E2E__?: HolonE2eDiagnostics;
  }
}

function snapshot(): HolonE2eSnapshot {
  const state = useRuntimeStore.getState();
  const conversationScope = state.selectedAgentId
    ? peekConversationScope(
        resolveConversationScopeKey(
          currentRemoteKey(getRuntimeConnectionConfig()),
          state.selectedAgentId,
          state.currentUser,
        ),
      )
    : null;
  return {
    conversationStatus: conversationScope
      ? {
          kind: conversationScope.status.kind,
          error: conversationScope.status.kind === "unsupported" ||
              conversationScope.status.kind === "recoverable_error" ||
              conversationScope.status.kind === "terminal_error"
            ? String(conversationScope.status.error ?? "")
            : undefined,
          hasView: conversationScope.view() !== null,
        }
      : undefined,
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

function ledgerStatus(agentId: string): HolonE2eLedgerStatus | null {
  const status = ledgerStatusForDiagnostics(agentId);
  if (!status) return null;
  return {
    durability: status.durability,
    ingestionState: status.state,
    ingestionError: status.lastError,
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
      [AGENT_SESSIONS_STORE, PENDING_HYDRATION_STORE],
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
    const pending = jobs.filter((job) => job.state !== "failed");
    const storeState = useRuntimeStore.getState();
    const status = ledgerStatusForDiagnostics(agentId);
    const readState = storeState.briefReadStateByAgentId[agentId];
    return {
      agentId,
      runtimeId: session.runtimeId,
      visibilityScopeId: session.visibilityScopeId,
      eventLogEpoch: session.eventLogEpoch,
      durability: status?.durability ?? "unknown",
      ingestionState: status?.state ?? "unknown",
      ingestionError: status?.lastError,
      ingestedThroughSeq: session.ingestedThroughSeq ?? 0,
      observedHeadSeq: session.observedHeadSeq ?? session.ingestedThroughSeq ?? 0,
      projectionReadyThroughSeq: session.projectionReadyThroughSeq ?? 0,
      pendingHydrationJobs: pending.length,
      failedHydrationJobs: jobs.length - pending.length,
      blockedByEventSeq: pending.length
        ? Math.min(...pending.map((job) => job.createdByEventSeq))
        : undefined,
      blockedReason: pending.length ? "pending_hydration" : undefined,
      readThroughEventSeq: readState?.read_through_event_seq,
      certainty: readState?.retention_gap ? "truncated" : readState ? "exact" : undefined,
      historyTruncatedBeforeSeq: readState?.retention_gap
        ? readState.oldest_retained_seq
        : undefined,
      unreadCount: readState?.unread_count,
    };
  } finally {
    db.close();
  }
}

async function ledgerPartitions(agentId: string): Promise<HolonE2eLedgerPartition[]> {
  const db = await requestResult(indexedDB.open(LEDGER_DB_NAME, LEDGER_DB_VERSION));
  try {
    const transaction = db.transaction(
      [AGENT_SESSIONS_STORE, RAW_EVENTS_STORE],
      "readonly",
    );
    const sessions = (await requestResult(
      transaction.objectStore(AGENT_SESSIONS_STORE).getAll(),
    ) as LedgerAgentSessionRecord[]).filter((candidate) => candidate.agentId === agentId);
    const rawEvents = await requestResult(
      transaction.objectStore(RAW_EVENTS_STORE).getAll(),
    ) as LedgerRawEventRecord[];
    const readState = useRuntimeStore.getState().briefReadStateByAgentId[agentId];
    return sessions.map((session) => {
      const sameScope = (candidate: {
        remoteKey: string;
        runtimeId: string;
        visibilityScopeId: string;
        eventLogEpoch: string;
        agentId: string;
      }) =>
        candidate.remoteKey === session.remoteKey
        && candidate.runtimeId === session.runtimeId
        && candidate.visibilityScopeId === session.visibilityScopeId
        && candidate.eventLogEpoch === session.eventLogEpoch
        && candidate.agentId === session.agentId;
      return {
        runtimeId: session.runtimeId,
        visibilityScopeId: session.visibilityScopeId,
        eventLogEpoch: session.eventLogEpoch,
        eventSeqs: rawEvents.filter(sameScope).map((event) => event.eventSeq).sort((a, b) => a - b),
        observedHeadSeq: session.observedHeadSeq ?? session.ingestedThroughSeq,
        readThroughEventSeq: readState?.read_through_event_seq,
        certainty: readState?.retention_gap ? "truncated" : readState ? "exact" : undefined,
      };
    });
  } finally {
    db.close();
  }
}

window.__HOLON_E2E__ = Object.freeze({
  snapshot,
  ledgerStatus,
  ledger: ledgerSnapshot,
  ledgerPartitions,
  subscribe(listener: (value: HolonE2eSnapshot) => void) {
    return useRuntimeStore.subscribe(() => listener(snapshot()));
  },
});
