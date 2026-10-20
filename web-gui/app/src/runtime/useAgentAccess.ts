import { useCallback, useEffect, useLayoutEffect, useMemo, useSyncExternalStore } from "react";

import { createRuntimeClient } from "./client";
import { getRuntimeConnectionConfig, useRuntimeStore } from "./runtime-store";
import { currentRemoteKey } from "./session-cache";
import { agentPresentationCacheKey } from "./agent-presentation-cache";
import { conversationCacheKey, resolveConversationScopeKey, useConversationScopeStore } from "./conversation-scope-store";
import {
  acquireAgentAccess, agentAccessKey, agentAccessRejection, agentAccessRevision, agentAccessSnapshot, rejectAgentAccess, retryAgentAccess,
  setAgentAccessContext, subscribeAgentAccess,
} from "./agent-access-records";
export type { AgentAccessStatus } from "./agent-access-records";

/** Cached history is presentation only; every route entry revalidates access. */
export function useAgentAccess(agentId: string | undefined, expectedIncarnation?: number) {
  const connection = useRuntimeStore((state) => state.bootstrap.connection);
  const user = useRuntimeStore((state) => state.currentUser);
  const userLoaded = useRuntimeStore((state) => state.currentUserLoaded);
  const authRejected = useRuntimeStore((state) => state.discovery?.freshness === "unauthorized" || state.globalStreamStatus === "unauthorized");
  const config = getRuntimeConnectionConfig();
  const client = useMemo(() => createRuntimeClient(config), [connection.mode, connection.baseUrl, config.token]);
  const contextKey = JSON.stringify([connection.mode, connection.baseUrl, config.token, userLoaded, connection.authRequired, authRejected, user?.authMethod, user?.userId]);
  const identityKey = JSON.stringify([contextKey, agentId]);
  const key = agentAccessKey(JSON.stringify([identityKey, expectedIncarnation]), identityKey, expectedIncarnation);
  const remoteKey = currentRemoteKey(connection);
  useSyncExternalStore(subscribeAgentAccess, agentAccessRevision);
  const recorded = agentAccessSnapshot(key);
  const scopeKey = agentId && recorded.agent?.incarnation !== undefined
    ? resolveConversationScopeKey(remoteKey, agentId, user, recorded.agent.incarnation) : "";
  const mirror = useConversationScopeStore((state) => state.scopes[scopeKey]);
  const rejectedError = mirror?.status.kind === "terminal_error" || mirror?.status.kind === "recoverable_error"
    ? mirror.status.error : undefined;
  const rejected = agentAccessRejection(rejectedError);
  // A pinned summary is enough to start read-only snapshot hydration. Waiting
  // for a warm scope here would prevent that scope from mounting after reload.
  const warm = user !== undefined && recorded.agent?.incarnation !== undefined;
  const access = rejected ? { ...recorded, status: rejected, agent: undefined }
    : recorded.status === "available" || warm ? recorded
    : { ...recorded, status: recorded.status === "revalidating" ? "loading" as const : recorded.status, agent: undefined };
  const parent = access.parent;
  const retry = useCallback(() => retryAgentAccess(key), [key]);
  const rejectInput = useCallback((error: unknown) => rejectAgentAccess(key, error), [key]);

  useLayoutEffect(() => {
    if (rejected) rejectAgentAccess(key, rejectedError);
  }, [key, rejected, rejectedError]);

  useEffect(() => {
    setAgentAccessContext(contextKey);
    if (!agentId || !userLoaded || connection.authRequired || authRejected) return;
    return acquireAgentAccess({
      key, identityKey, agentId, expectedIncarnation, client,
      scopeKey: (incarnation) => resolveConversationScopeKey(remoteKey, agentId, user, incarnation),
      cacheKey: (incarnation) => conversationCacheKey(remoteKey, user, incarnation),
      presentationCacheKey: agentPresentationCacheKey(remoteKey, user, agentId),
      rosterCacheKey: agentPresentationCacheKey(remoteKey, user),
    });
  }, [key, identityKey, contextKey, agentId, expectedIncarnation, client, remoteKey, user?.authMethod, user?.userId, userLoaded, connection.authRequired, authRejected]);

  return { ...access, parentTaskId: access.parentAgentId ? parent?.taskId : undefined, retry, rejectInput };
}
