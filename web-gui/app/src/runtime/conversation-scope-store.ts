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
import { createRuntimeClient } from "./client";
import { pinConversationClient } from "./incarnation-conversation-client";
import { conversationNetworkGate } from "./conversation-network-gate";

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

function withIncarnation(key: string, incarnation: number | undefined): string {
  return incarnation === undefined ? key : `${key}#incarnation:${incarnation}`;
}

export function conversationCacheKey(remoteKey: string, user: { authMethod: string; userId: string } | undefined, incarnation?: number): string | undefined {
  return user ? withIncarnation(`${remoteKey}#${JSON.stringify([user.authMethod, user.userId])}`, incarnation) : undefined;
}

/** One identity-aware key for page ownership, read-marker gates and diagnostics. */
export function resolveConversationScopeKey(
  remoteKey: string,
  agentId: string,
  user: { authMethod: string; userId: string } | undefined,
  incarnation?: number,
): ConversationScopeKey {
  return conversationScopeKey(conversationCacheKey(remoteKey, user) ?? `${remoteKey}#memory`, agentId, incarnation);
}

export interface ConversationScopeHandle {
  readonly key: ConversationScopeKey;
  readonly controller: ConversationController;
}

interface ConversationScopeMirror {
  readonly status: ConversationStatus;
  readonly view: ConversationStateView | null;
  /** Presentation only; never used as a checkpoint or a read-marker gate. */
  readonly displayView: ConversationStateView | null;
  readonly version: number;
  /** A historical viewport is not evidence that the latest head was read. */
  readonly navigationActive?: boolean;
  readonly browsing?: boolean;
}

interface ConversationStoreState {
  readonly scopes: Readonly<Record<ConversationScopeKey, ConversationScopeMirror>>;
}

export const EMPTY_CONVERSATION_MIRROR: ConversationScopeMirror = {
  status: { kind: "idle" },
  view: null,
  displayView: null,
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

export function setConversationNavigationActive(key: ConversationScopeKey, active: boolean): void {
  const previous = conversationScopeSnapshot(key);
  if (Boolean(previous.navigationActive) === active) return;
  useConversationScopeStore.setState((state) => ({
    scopes: { ...state.scopes, [key]: { ...previous, navigationActive: active, version: previous.version + 1 } },
  }));
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
  incarnation?: number,
): ConversationScopeKey {
  return withIncarnation(`${connectionKey}::${agentId}`, incarnation);
}

interface RegistryEntry {
  readonly controller: ConversationController;
  readonly network: ReturnType<typeof conversationNetworkGate>;
  readonly unsubscribe: () => void;
  refCount: number;
  writableCount: number;
  viewport?: ConversationViewport;
}

export interface ConversationViewport {
  readonly scrollTop: number;
  readonly stickToBottom: boolean;
  readonly anchors: readonly { readonly anchor: string; readonly offset: number }[];
}

export function conversationViewport(key: string): ConversationViewport | undefined {
  return registry.get(key)?.viewport;
}

export function saveConversationViewport(key: string, viewport: ConversationViewport): void {
  const entry = registry.get(key);
  if (entry) entry.viewport = viewport;
}

const registry = new Map<ConversationScopeKey, RegistryEntry>();

/**
 * How many released (idle) scopes keep their controller data alive for
 * instant switching back. Streams are paused; the bound limits memory usage.
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
  readonly incarnation?: number;
  /** Attach to history without starting or accepting network work. */
  readonly readOnly?: boolean;
  readonly remoteId?: string;
  /** Verified identity-scoped storage key; absent means memory-only. */
  readonly cacheKey?: string;
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
 * (stream paused) up to `CONVERSATION_SCOPE_KEEP_ALIVE`; older idle scopes
 * are disposed in LRU order.
 */
export function acquireConversationScope(
  options: AcquireConversationScopeOptions,
): ConversationScopeHandle {
  const existing = registry.get(options.key);
  if (existing !== undefined) {
    const wasPaused = existing.writableCount === 0;
    existing.refCount += 1;
    if (!options.readOnly) existing.writableCount += 1;
    removeFromIdleOrder(options.key);
    if (wasPaused && existing.writableCount > 0 && isPageVisible()) {
      existing.network.resume();
      existing.controller.start();
    }
    return { key: options.key, controller: existing.controller };
  }
  const factory = options.clientFactory ?? defaultClientFactory;
  const client = factory(options);
  const identityClient = options.incarnation === undefined ? undefined : createRuntimeClient(options);
  const network = conversationNetworkGate(identityClient === undefined || options.incarnation === undefined ? client : pinConversationClient(client, {
    agentId: options.agentId,
    incarnation: options.incarnation,
    getAgentIdentity: (agentId) => identityClient.getAgentIdentity(agentId),
  }));
  const controller = new ConversationController({
    ...(options.controllerOptions ?? {}),
    // Default-inject the persistent brief/snapshot caches unless the caller
    // supplied its own (tests inject in-memory fakes). Storage-less
    // environments silently degrade to memory-only caching inside the
    // adapters.
    ...(options.cacheKey !== undefined && options.controllerOptions?.briefCache === undefined
      ? {
          briefCache: createConversationBriefCache(
            options.cacheKey,
            options.agentId,
          ),
        }
      : {}),
    ...(options.cacheKey !== undefined && options.controllerOptions?.snapshotCache === undefined
      ? {
          snapshotCache: createConversationSnapshotCache(
            options.cacheKey,
            options.agentId,
          ),
        }
      : {}),
    client: network.client,
    agentId: options.agentId,
    ...(options.remoteId === undefined ? {} : { remoteId: options.remoteId }),
  });
  const unsubscribe = controller.subscribe(() => {
    publishScopeSnapshot(options.key, controller);
  });
  registry.set(options.key, {
    controller,
    network,
    unsubscribe,
    refCount: 1,
    writableCount: options.readOnly ? 0 : 1,
  });
  publishScopeSnapshot(options.key, controller);
  if (!options.readOnly && isPageVisible()) {
    network.resume();
    controller.start();
  }
  evictIdleScopes();
  return { key: options.key, controller };
}

/** Drop one holder; the last release makes the scope idle but kept alive. */
export function releaseConversationScope(key: ConversationScopeKey, readOnly = false): void {
  const entry = registry.get(key);
  if (entry === undefined) return;
  if (entry.refCount === 0) return;
  entry.refCount -= 1;
  if (!readOnly) entry.writableCount = Math.max(0, entry.writableCount - 1);
  if (entry.writableCount === 0) {
    entry.network.pause();
    entry.controller.pause();
  }
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
export function disposeAllConversationScopes(persist = true): void {
  for (const key of [...registry.keys()]) {
    const entry = registry.get(key);
    if (entry !== undefined) {
      disposeScope(key, entry, persist);
    }
  }
  idleOrder.length = 0;
}

export function disposeConversationScope(key: ConversationScopeKey, persist = true): void {
  const entry = registry.get(key);
  if (entry) disposeScope(key, entry, persist);
  else useConversationScopeStore.setState((state) => {
    const scopes = { ...state.scopes };
    delete scopes[key];
    return { scopes };
  });
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

function disposeScope(key: ConversationScopeKey, entry: RegistryEntry, persist = true): void {
  registry.delete(key);
  removeFromIdleOrder(key);
  entry.network.dispose();
  entry.unsubscribe();
  entry.controller.dispose(persist);
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

function isPageVisible(): boolean {
  return typeof document === "undefined" || document.visibilityState !== "hidden";
}

if (typeof document !== "undefined") {
  document.addEventListener("visibilitychange", () => {
    for (const entry of registry.values()) {
      if (!isPageVisible()) {
        entry.network.pause();
        entry.controller.pause();
      } else if (entry.writableCount > 0) {
        entry.network.resume();
        entry.controller.start();
      }
    }
  });
}

function publishScopeSnapshot(
  key: ConversationScopeKey,
  controller: ConversationController,
): void {
  const previous =
    useConversationScopeStore.getState().scopes[key] ?? EMPTY_CONVERSATION_MIRROR;
  const view = controller.view();
  const displayView = recoveryDisplayView(previous.displayView, controller.browsingView() ?? view, controller.status);
  useConversationScopeStore.setState((state) => ({
    scopes: {
      ...state.scopes,
      [key]: {
        status: controller.status,
        view,
        displayView,
        navigationActive: previous.navigationActive,
        browsing: controller.browseState() !== null,
        version: previous.version + 1,
      },
    },
  }));
}

/** Retain only transport-recovery frames, never a rejected identity or access. */
export function recoveryDisplayView(
  previous: ConversationStateView | null,
  next: ConversationStateView,
  status: ConversationStatus,
): ConversationStateView {
  const recoverable = next.reset_reason != null && [
    "retention_expired", "replay_limit_exceeded", "slow_consumer", "stream_recovery_failed",
  ].includes(next.reset_reason);
  if (previous?.scope && !next.scope && recoverable
    && ["ready", "loading", "reconnecting", "paused"].includes(status.kind)) {
    return { ...previous, checkpoint: null, has_more: false, next_before_cursor: null, reset_reason: next.reset_reason };
  }
  return next;
}
