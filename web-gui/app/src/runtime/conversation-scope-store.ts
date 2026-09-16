import { create } from "zustand";
import {
  ConversationClient,
  ConversationController,
  type ConversationClientLike,
  type ConversationControllerOptions,
  type ConversationStateView,
  type ConversationStatus,
} from "@holon/conversation-sdk";

import { createConversationBriefCache } from "./conversation-brief-cache";
import { createConversationSnapshotCache } from "./conversation-snapshot-cache";

/**
 * Runtime connection inputs needed to build a conversation client. Mirrors
 * the runtime client's base URL and bearer semantics without importing the
 * full runtime store.
 */
export interface ConversationConnectionOptions {
  readonly baseUrl: string;
  readonly token?: string;
  readonly fetchImpl?: typeof fetch;
}

export type ConversationScopeKey = string;

export interface ConversationScopeHandle {
  readonly key: ConversationScopeKey;
  readonly controller: ConversationController;
}

interface ConversationScopeMirror {
  readonly status: ConversationStatus;
  readonly view: ConversationStateView | null;
  readonly version: number;
}

interface ConversationStoreState {
  readonly scopes: Readonly<Record<ConversationScopeKey, ConversationScopeMirror>>;
}

export const EMPTY_CONVERSATION_MIRROR: ConversationScopeMirror = {
  status: { kind: "idle" },
  view: null,
  version: 0,
};

export const useConversationScopeStore = create<ConversationStoreState>(() => ({
  scopes: {},
}));

export function conversationScopeSnapshot(
  key: ConversationScopeKey,
): ConversationScopeMirror {
  return useConversationScopeStore.getState().scopes[key] ?? EMPTY_CONVERSATION_MIRROR;
}

export function subscribeConversationScope(
  key: ConversationScopeKey,
  listener: () => void,
): () => void {
  let lastVersion = -1;
  return useConversationScopeStore.subscribe((state) => {
    const version = state.scopes[key]?.version ?? 0;
    if (version !== lastVersion) {
      lastVersion = version;
      listener();
    }
  });
}

export function conversationScopeKey(
  connectionKey: string,
  agentId: string,
): ConversationScopeKey {
  return `${connectionKey}::${agentId}`;
}

interface RegistryEntry {
  readonly controller: ConversationController;
  readonly unsubscribe: () => void;
  refCount: number;
}

const registry = new Map<ConversationScopeKey, RegistryEntry>();

/**
 * How many released (idle) scopes keep their controller and stream alive for
 * instant switching back. Bounded so daemon-side concurrent SSE connections
 * stay predictable.
 */
export const CONVERSATION_SCOPE_KEEP_ALIVE = 3;

// Least-recently-released idle scope first; active scopes are absent.
const idleOrder: ConversationScopeKey[] = [];

export type ConversationClientFactory = (
  connection: ConversationConnectionOptions,
) => ConversationClientLike;

const defaultClientFactory: ConversationClientFactory = (connection) =>
  new ConversationClient({
    baseUrl: connection.baseUrl,
    fetch: connection.fetchImpl ?? fetch,
    ...(connection.token === undefined
      ? {}
      : { bearerToken: connection.token }),
  });

export interface AcquireConversationScopeOptions
  extends ConversationConnectionOptions {
  readonly key: ConversationScopeKey;
  readonly agentId: string;
  readonly remoteId?: string;
  readonly controllerOptions?: Omit<
    ConversationControllerOptions,
    "client" | "agentId" | "remoteId"
  >;
  readonly clientFactory?: ConversationClientFactory;
}

/**
 * Acquire (or attach to) the conversation controller for one
 * connection+agent scope. The controller starts immediately; callers release
 * it when their React surface unmounts. Recently released scopes stay alive
 * (stream attached) up to `CONVERSATION_SCOPE_KEEP_ALIVE`; older idle scopes
 * are disposed in LRU order.
 */
export function acquireConversationScope(
  options: AcquireConversationScopeOptions,
): ConversationScopeHandle {
  const existing = registry.get(options.key);
  if (existing !== undefined) {
    const wasIdle = existing.refCount === 0;
    existing.refCount += 1;
    if (wasIdle) {
      removeFromIdleOrder(options.key);
      restartIdleScopeIfNeeded(existing.controller);
    }
    return { key: options.key, controller: existing.controller };
  }
  const factory = options.clientFactory ?? defaultClientFactory;
  const controller = new ConversationController({
    ...(options.controllerOptions ?? {}),
    // Default-inject the persistent brief/snapshot caches unless the caller
    // supplied its own (tests inject in-memory fakes). Storage-less
    // environments silently degrade to memory-only caching inside the
    // adapters.
    ...(options.controllerOptions?.briefCache === undefined
      ? {
          briefCache: createConversationBriefCache(
            options.remoteId ?? "",
            options.agentId,
          ),
        }
      : {}),
    ...(options.controllerOptions?.snapshotCache === undefined
      ? {
          snapshotCache: createConversationSnapshotCache(
            options.remoteId ?? "",
            options.agentId,
          ),
        }
      : {}),
    client: factory(options),
    agentId: options.agentId,
    ...(options.remoteId === undefined ? {} : { remoteId: options.remoteId }),
  });
  const unsubscribe = controller.subscribe(() => {
    publishScopeSnapshot(options.key, controller);
  });
  registry.set(options.key, {
    controller,
    unsubscribe,
    refCount: 1,
  });
  publishScopeSnapshot(options.key, controller);
  controller.start();
  evictIdleScopes();
  return { key: options.key, controller };
}

/** Drop one holder; the last release makes the scope idle but kept alive. */
export function releaseConversationScope(key: ConversationScopeKey): void {
  const entry = registry.get(key);
  if (entry === undefined) return;
  entry.refCount -= 1;
  if (entry.refCount > 0) return;
  entry.refCount = 0;
  idleOrder.push(key);
  evictIdleScopes();
}

export function peekConversationScope(
  key: ConversationScopeKey,
): ConversationController | null {
  return registry.get(key)?.controller ?? null;
}

/** Registered scope count, including idle keep-alive scopes. */
export function activeConversationScopeCount(): number {
  return registry.size;
}

/** Idle (released but kept alive) scope count. */
export function idleConversationScopeCount(): number {
  return idleOrder.length;
}

/** Dispose every registered scope, including idle keep-alive entries. */
export function disposeAllConversationScopes(): void {
  for (const key of [...registry.keys()]) {
    const entry = registry.get(key);
    if (entry !== undefined) {
      disposeScope(key, entry);
    }
  }
  idleOrder.length = 0;
}

function evictIdleScopes(): void {
  while (idleOrder.length > CONVERSATION_SCOPE_KEEP_ALIVE) {
    const key = idleOrder.shift();
    if (key === undefined) break;
    const entry = registry.get(key);
    if (entry === undefined || entry.refCount !== 0) continue;
    disposeScope(key, entry);
  }
}

function disposeScope(key: ConversationScopeKey, entry: RegistryEntry): void {
  registry.delete(key);
  entry.unsubscribe();
  entry.controller.dispose();
  useConversationScopeStore.setState((state) => {
    if (!(key in state.scopes)) return state;
    const scopes = { ...state.scopes };
    delete scopes[key];
    return { scopes };
  });
}

function removeFromIdleOrder(key: ConversationScopeKey): void {
  const index = idleOrder.indexOf(key);
  if (index !== -1) {
    idleOrder.splice(index, 1);
  }
}

// A kept-alive controller may have ended in a terminal state while idle;
// remounting the surface should restart it like a freshly created scope.
function restartIdleScopeIfNeeded(controller: ConversationController): void {
  const kind = controller.status.kind;
  if (
    kind === "terminal_error" ||
    kind === "recoverable_error" ||
    kind === "unsupported"
  ) {
    try {
      controller.retry();
    } catch {
      // Disposed concurrently; the acquire path will not observe it.
    }
  }
}

function publishScopeSnapshot(
  key: ConversationScopeKey,
  controller: ConversationController,
): void {
  const previous =
    useConversationScopeStore.getState().scopes[key] ?? EMPTY_CONVERSATION_MIRROR;
  useConversationScopeStore.setState((state) => ({
    scopes: {
      ...state.scopes,
      [key]: {
        status: controller.status,
        view: controller.view(),
        version: previous.version + 1,
      },
    },
  }));
}
