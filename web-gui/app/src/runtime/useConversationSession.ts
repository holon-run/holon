import { useCallback, useEffect, useMemo } from "react";

import type {
  BriefRecord,
  ConversationBriefLoadState,
  ConversationController,
  ConversationDetailLoadState,
} from "@holon/conversation-sdk";

import { resolveRuntimeApiBase } from "./client";
import {
  acquireConversationScope,
  conversationScopeKey,
  conversationScopeSnapshot,
  EMPTY_CONVERSATION_MIRROR,
  peekConversationScope,
  releaseConversationScope,
  subscribeConversationScope,
  useConversationScopeStore,
} from "./conversation-scope-store";
import {
  buildConversationSessionModel,
  type ConversationSessionModel,
} from "./conversation-view-model";
import { getRuntimeConnectionConfig, useRuntimeStore } from "./runtime-store";
import { currentRemoteKey } from "./session-cache";

export interface UseConversationSessionResult {
  readonly model: ConversationSessionModel;
  readonly scopeKey: string | null;
  readonly controller: ConversationController | null;
  retry: () => void;
  loadOlderHistory: () => void;
  loadDetail: (turnId: string) => void;
  loadOlderActivities: (turnId: string) => void;
  loadBrief: (briefId: string) => void;
  briefRecord: (briefId: string) => BriefRecord | null;
  briefState: (briefId: string) => ConversationBriefLoadState | null;
  detailState: (turnId: string) => ConversationDetailLoadState;
}

/**
 * Page-level conversation lifecycle: acquires the SDK controller for the
 * active connection+agent scope on mount, releases it on unmount or scope
 * switch, and exposes a render-ready session model. Content projection is
 * owned by the SDK; this hook only adapts transport and UI state.
 */
export function useConversationSession(
  agentId: string | undefined,
): UseConversationSessionResult {
  const connection = useRuntimeStore((state) => state.bootstrap.connection);
  const remoteKey = currentRemoteKey({
    mode: connection.mode,
    baseUrl: connection.baseUrl,
  });
  const apiBase = resolveRuntimeApiBase({
    mode: connection.mode,
    baseUrl: connection.baseUrl,
  });
  const scopeKey =
    agentId !== undefined && agentId.length > 0 && apiBase !== undefined
      ? conversationScopeKey(remoteKey, agentId)
      : null;

  useEffect(() => {
    if (scopeKey === null || agentId === undefined) return;
    const config = getRuntimeConnectionConfig();
    acquireConversationScope({
      key: scopeKey,
      agentId,
      remoteId: remoteKey,
      baseUrl: apiBase ?? "",
      ...(config.token === undefined ? {} : { token: config.token }),
    });
    return () => {
      releaseConversationScope(scopeKey);
    };
  }, [scopeKey, agentId, remoteKey, apiBase]);

  // Subscribe to scope snapshots so brief/detail/history request states and
  // view revisions re-render even while the controller object is stable.
  useEffect(() => {
    if (scopeKey === null) return;
    return subscribeConversationScope(scopeKey, () => {
      useConversationScopeStore.getState();
    });
  }, [scopeKey]);

  const mirror = useConversationScopeStore((state) =>
    scopeKey === null ? undefined : state.scopes[scopeKey],
  );
  const controller = scopeKey !== null ? peekConversationScope(scopeKey) : null;
  const snapshot = mirror ?? conversationScopeSnapshot(scopeKey ?? "");
  const version = snapshot.version;

  const model = useMemo(() => {
    const view = snapshot.view;
    const briefs = new Map<string, BriefRecord>();
    const briefLoadStates = new Map<string, ConversationBriefLoadState>();
    const detailLoadStates = new Map<string, ConversationDetailLoadState>();
    if (controller !== null) {
      const briefIds = new Set<string>();
      for (const turn of view?.turns ?? []) {
        for (const briefId of turn.brief_ids) {
          briefIds.add(briefId);
        }
      }
      for (const briefId of briefIds) {
        const state = controller.briefState(briefId);
        if (state === null) continue;
        briefLoadStates.set(briefId, state);
        if (state.kind === "ready") {
          briefs.set(briefId, state.brief);
        }
      }
      for (const turn of view?.turns ?? []) {
        const state = controller.detailState(turn.turn_id);
        if (state.kind !== "idle") {
          detailLoadStates.set(turn.turn_id, state);
        }
      }
    }
    return buildConversationSessionModel({
      status: snapshot.status,
      view,
      historyState: controller?.historyState() ?? { kind: "idle" },
      briefs,
      briefLoadStates,
      detailLoadStates,
    });
    // `version` covers controller-side mutations; `controller` and
    // `snapshot` are stable between version bumps.
  }, [snapshot, controller, version]);

  const loadOlderHistory = useCallback(() => {
    if (controller === null) return;
    void controller.loadOlderHistory();
  }, [controller]);

  const loadDetail = useCallback(
    (turnId: string) => {
      if (controller === null) return;
      void controller.loadDetail(turnId);
    },
    [controller],
  );

  const loadOlderActivities = useCallback(
    (turnId: string) => {
      if (controller === null) return;
      void controller.loadOlderActivities(turnId);
    },
    [controller],
  );

  const loadBrief = useCallback(
    (briefId: string) => {
      if (controller === null) return;
      void controller.loadBrief(briefId);
    },
    [controller],
  );

  const briefRecord = useCallback(
    (briefId: string) => {
      if (controller === null) return null;
      const state = controller.briefState(briefId);
      if (state === null || state.kind !== "ready") return null;
      return state.brief;
    },
    [controller],
  );

  const briefState = useCallback(
    (briefId: string) => controller?.briefState(briefId) ?? null,
    [controller],
  );

  const detailState = useCallback(
    (turnId: string): ConversationDetailLoadState =>
      controller?.detailState(turnId) ?? { kind: "idle" },
    [controller],
  );

  const retry = useCallback(() => {
    controller?.retry();
  }, [controller]);

  return {
    model,
    scopeKey,
    controller,
    retry,
    loadOlderHistory,
    loadDetail,
    loadOlderActivities,
    loadBrief,
    briefRecord,
    briefState,
    detailState,
  };
}

export const CONVERSATION_MIRROR_FALLBACK = EMPTY_CONVERSATION_MIRROR;
