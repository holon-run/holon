import { useCallback, useEffect, useLayoutEffect, useMemo, useRef } from "react";
import { ConversationCapabilityError } from "@holon/conversation-sdk";

import type {
  BriefRecord,
  ConversationBriefLoadState,
  ConversationController,
  ConversationDetailLoadState,
} from "@holon/conversation-sdk";

import { resolveRuntimeApiBase } from "./client";
import {
  acquireConversationScope,
  conversationCacheKey,
  resolveConversationScopeKey,
  conversationScopeSnapshot,
  peekConversationScope,
  releaseConversationScope,
  setConversationNavigationActive,
  subscribeConversationScope,
  useConversationScopeStore,
} from "./conversation-scope-store";
import {
  buildConversationSessionModel,
  type ConversationSessionModel,
} from "./conversation-view-model";
import { getRuntimeConnectionConfig, retryPendingReadMarker, useRuntimeStore } from "./runtime-store";
import { currentRemoteKey } from "./session-cache";
import type { ConversationLocation, ConversationNavigation } from "../features/agent/conversation-location";

const NO_PENDING_OPERATOR_PROMPTS: readonly never[] = [];

export interface UseConversationSessionResult {
  readonly model: ConversationSessionModel;
  readonly scopeKey: string | null;
  readonly controller: ConversationController | null;
  readonly conversationReady: boolean;
  readonly location?: ConversationLocation;
  readonly browsing: boolean;
  readonly hasNewerHistory: boolean;
  readonly newerHistoryState: ReturnType<ConversationController["historyState"]>;
  locateSource: () => void;
  returnToLatest: () => void;
  loadNewerHistory: () => void;
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
  navigation?: ConversationNavigation,
): UseConversationSessionResult {
  const currentUser = useRuntimeStore((state) => state.currentUser);
  const currentUserLoaded = useRuntimeStore((state) => state.currentUserLoaded);
  const connection = useRuntimeStore((state) => state.bootstrap.connection);
  const pendingOperatorPrompts = useRuntimeStore((state) =>
    agentId === undefined
      ? NO_PENDING_OPERATOR_PROMPTS
      : state.sessionsByAgentId[agentId]?.pendingOperatorPrompts ?? NO_PENDING_OPERATOR_PROMPTS,
  );
  const pruneOperatorPrompts = useRuntimeStore((state) => state.pruneOperatorPrompts);
  const remoteKey = currentRemoteKey({
    mode: connection.mode,
    baseUrl: connection.baseUrl,
  });
  const apiBase = resolveRuntimeApiBase({
    mode: connection.mode,
    baseUrl: connection.baseUrl,
  });
  // Never read another login's persisted content while identity is unknown.
  const cacheKey = conversationCacheKey(remoteKey, currentUser);
  const scopeKey =
    currentUserLoaded && !connection.authRequired && agentId !== undefined && agentId.length > 0 && apiBase !== undefined
      ? resolveConversationScopeKey(remoteKey, agentId, currentUser)
      : null;

  useEffect(() => {
    if (scopeKey === null || agentId === undefined) return;
    const config = getRuntimeConnectionConfig();
    acquireConversationScope({
      key: scopeKey,
      agentId,
      remoteId: remoteKey,
      cacheKey,
      baseUrl: apiBase ?? "",
      ...(config.token === undefined ? {} : { token: config.token }),
    });
    return () => {
      releaseConversationScope(scopeKey);
    };
  }, [scopeKey, agentId, remoteKey, apiBase, cacheKey]);

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
  const bootstrapped = snapshot.status.kind === "ready" && snapshot.view?.scope != null && snapshot.view.reset_reason === null;
  const browse = controller?.browseState() ?? null;
  const boundNavigationRuntime = useRef<{ requestId: number; runtimeId: string } | null>(null);
  const targetRuntime = navigation?.runtimeId
    ?? (boundNavigationRuntime.current?.requestId === navigation?.requestId ? boundNavigationRuntime.current?.runtimeId : undefined);
  const runtimeMismatch = targetRuntime !== undefined && snapshot.view?.scope != null && targetRuntime !== snapshot.view.scope.runtime_id;
  const browsing = browse !== null;
  const navigationActive = navigation !== undefined;
  const conversationReady = bootstrapped && !browsing && !navigation && !runtimeMismatch;
  const lastNavigation = useRef<{ controller: ConversationController; requestId: number } | null>(null);

  useLayoutEffect(() => {
    if (scopeKey === null) return;
    setConversationNavigationActive(scopeKey, navigationActive);
    return () => { setConversationNavigationActive(scopeKey, false); };
  }, [scopeKey, navigationActive]);

  // Cancellation belongs to route intent, not transient stream status changes.
  useEffect(() => () => {
    controller?.returnToLatest();
    lastNavigation.current = null;
  }, [controller, navigation?.requestId]);

  useEffect(() => {
    if (!controller || !navigation?.sourceRef || !bootstrapped || runtimeMismatch) return;
    if (lastNavigation.current?.controller === controller && lastNavigation.current.requestId === navigation.requestId) return;
    boundNavigationRuntime.current = { requestId: navigation.requestId, runtimeId: snapshot.view!.scope!.runtime_id };
    lastNavigation.current = { controller, requestId: navigation.requestId };
    void controller.locateSource(navigation.sourceRef);
  }, [controller, navigation?.requestId, navigation?.sourceRef, bootstrapped, runtimeMismatch, snapshot.view?.scope?.runtime_id]);

  const location = useMemo<ConversationLocation | undefined>(() => {
    if (!navigation) return undefined;
    if (runtimeMismatch) return { requestId: navigation.requestId, kind: "error", reason: "runtime" };
    if (!navigation.sourceRef) return { requestId: navigation.requestId, kind: "error", reason: "legacy" };
    if (!browse || browse.sourceRef !== navigation.sourceRef || browse.status === "resolving") {
      return { requestId: navigation.requestId, kind: "loading" };
    }
    if (browse.status === "error") return {
      requestId: navigation.requestId, kind: "error",
      reason: browse.error instanceof ConversationCapabilityError ? "unsupported" : undefined,
      error: browse.error instanceof ConversationCapabilityError ? undefined
        : browse.error instanceof Error ? browse.error.message : undefined,
    };
    if (browse.status === "source_only") return { requestId: navigation.requestId, kind: "source_only" };
    const target = browse.target;
    if (!target) return { requestId: navigation.requestId, kind: "loading" };
    return {
      requestId: navigation.requestId, kind: "ready",
      anchor: target.kind === "brief" ? `brief:${target.brief_id}`
        : target.kind === "activity" ? `activity:${target.activity_id}`
        : target.kind === "turn" ? `turn:${target.turn_id}` : `input:${target.message_id}`,
      turnId: "turn_id" in target ? target.turn_id : undefined,
    };
  }, [navigation, browse, runtimeMismatch]);

  // Locally echoed prompts retire once the server view carries their message
  // id (pending input or assigned turn input). Stale entries age out via the
  // store-side TTL.
  useEffect(() => {
    if (agentId === undefined || pendingOperatorPrompts.length === 0) return;
    const view = snapshot.displayView;
    if (!view?.scope) return;
    const covered = new Set<string>([
      ...view.pending_inputs.map((input) => input.message_id),
      ...view.turns.flatMap((turn) => turn.inputs.map((input) => input.message_id)),
    ]);
    const confirmed = pendingOperatorPrompts
      .map((entry) => entry.messageId)
      .filter((messageId): messageId is string => messageId !== undefined && covered.has(messageId));
    // Cheap no-op when nothing is covered; also ages out stale entries.
    pruneOperatorPrompts(agentId, confirmed);
  }, [agentId, snapshot, pendingOperatorPrompts, pruneOperatorPrompts]);

  // A scope that becomes ready can retry a pending server read-state update.
  useEffect(() => {
    if (!conversationReady || agentId === undefined) return;
    void retryPendingReadMarker(agentId);
  }, [conversationReady, agentId]);

  const model = useMemo(() => {
    const view = snapshot.displayView;
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
      locatedTurnId: location?.turnId,
      localPendingPrompts: browse && !browse.liveConnected ? [] : pendingOperatorPrompts,
      historyState: controller?.historyState() ?? { kind: "idle" },
      briefs,
      briefLoadStates,
      detailLoadStates,
    });
    // `version` covers controller-side mutations; `controller` and
    // `snapshot` are stable between version bumps.
  }, [snapshot, controller, version, pendingOperatorPrompts, browse, location?.turnId]);

  const loadOlderHistory = useCallback(() => {
    if (controller === null || controller.view().reset_reason !== null) return;
    void controller.loadOlderHistory();
  }, [controller]);

  const loadNewerHistory = useCallback(() => { void controller?.loadNewerHistory(); }, [controller]);
  const locateSource = useCallback(() => {
    if (navigation?.sourceRef && controller && !runtimeMismatch) void controller.locateSource(navigation.sourceRef);
  }, [controller, navigation?.sourceRef, runtimeMismatch]);
  const returnToLatest = useCallback(() => { controller?.returnToLatest(); }, [controller]);

  const loadDetail = useCallback(
    (turnId: string) => {
      if (controller === null || controller.view().reset_reason !== null) return;
      void controller.loadDetail(turnId);
    },
    [controller],
  );

  const loadOlderActivities = useCallback(
    (turnId: string) => {
      if (controller === null || controller.view().reset_reason !== null) return;
      void controller.loadOlderActivities(turnId);
    },
    [controller],
  );

  const loadBrief = useCallback(
    (briefId: string) => {
      if (controller === null || controller.view().reset_reason !== null) return;
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
    conversationReady,
    location,
    browsing,
    hasNewerHistory: browse?.hasNewer ?? false,
    newerHistoryState: browse?.newer ?? { kind: "idle" },
    locateSource,
    returnToLatest,
    loadNewerHistory,
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
