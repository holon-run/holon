import {
  getRuntimeConnectionConfig,
  useRuntimeStore,
} from "../runtime/runtime-store";
import {
  resolveConversationScopeKey,
  peekConversationScope,
} from "../runtime/conversation-scope-store";
import { currentRemoteKey } from "../runtime/session-cache";
import { agentAccessKey, agentAccessSnapshot } from "../runtime/agent-access-records";
import type { BriefReadStateDto } from "../runtime/client";
export interface HolonE2eSnapshot {
  conversationStatus?: {
    kind: string;
    error?: string;
    hasView: boolean;
  };
  route: string;
  bootstrapLoading: boolean;
  bootstrapError?: string;
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

export interface HolonE2eConversation {
  status: string;
  eventLogEpoch?: string;
  throughSeq?: number;
  turnIds: string[];
  pendingInputIds: string[];
}

export interface HolonE2eDiagnostics {
  conversation(agentId: string): HolonE2eConversation | null;
  readState(agentId: string): BriefReadStateDto | null;
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

function selectedConversationScope() {
  const state = useRuntimeStore.getState();
  const config = getRuntimeConnectionConfig();
  const connection = state.bootstrap.connection;
  const authRejected = state.discovery?.freshness === "unauthorized";
  const contextKey = JSON.stringify([
    connection.mode, connection.baseUrl, config.token, state.currentUserLoaded,
    connection.authRequired, authRejected, state.currentUser?.authMethod, state.currentUser?.userId,
  ]);
  const identityKey = JSON.stringify([contextKey, state.selectedAgentId]);
  const incarnationParam = new URLSearchParams(window.location.search).get("incarnation");
  const expectedIncarnation = incarnationParam === null ? undefined
    : /^\d+$/.test(incarnationParam) && Number.isSafeInteger(Number(incarnationParam)) && Number(incarnationParam) > 0
      ? Number(incarnationParam) : -1;
  const access = agentAccessSnapshot(agentAccessKey(
    JSON.stringify([identityKey, expectedIncarnation]), identityKey, expectedIncarnation,
  ));
  return state.selectedAgentId
    ? peekConversationScope(
        resolveConversationScopeKey(
          currentRemoteKey(getRuntimeConnectionConfig()),
          state.selectedAgentId,
          state.currentUser,
          access.agent?.incarnation,
        ),
      )
    : null;
}

function snapshot(): HolonE2eSnapshot {
  const state = useRuntimeStore.getState();
  const conversationScope = selectedConversationScope();
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
  readState: (agentId: string) =>
    useRuntimeStore.getState().briefReadStateByAgentId[agentId] ?? null,
  // Compatibility tombstones: never open or resurrect the retired ledger DB.
  ledgerStatus: () => ({ durability: "unavailable", ingestionState: "retired" }),
  ledger: async () => null,
  ledgerPartitions: async () => [],
  conversation(agentId: string) {
    const state = useRuntimeStore.getState();
    if (state.selectedAgentId !== agentId) return null;
    const controller = selectedConversationScope();
    if (!controller) return null;
    const view = controller.view();
    return {
      status: controller.status.kind,
      eventLogEpoch: view?.scope?.event_log_epoch,
      throughSeq: view?.through_seq ?? undefined,
      turnIds: view?.turns.map((turn) => turn.turn_id) ?? [],
      pendingInputIds: view?.pending_inputs.map((input) => input.message_id) ?? [],
    };
  },
  subscribe(listener: (value: HolonE2eSnapshot) => void) {
    return useRuntimeStore.subscribe(() => listener(snapshot()));
  },
});
