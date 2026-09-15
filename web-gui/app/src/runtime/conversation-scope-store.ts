import { create } from "zustand";
import {
  ConversationClient,
  ConversationController,
  type ConversationControllerOptions,
  type ConversationStateView,
  type ConversationStatus,
} from "@holon/conversation-sdk";

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

export type ConversationClientFactory = (
  connection: ConversationConnectionOptions,
) => ConversationClient;

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
 * it when their React surface unmounts, which disposes the controller once
 * the last holder is gone.
 */
export function acquireConversationScope(
  options: AcquireConversationScopeOptions,
): ConversationScopeHandle {
  const existing = registry.get(options.key);
  if (existing !== undefined) {
    existing.refCount += 1;
    return { key: options.key, controller: existing.controller };
  }
  const factory = options.clientFactory ?? defaultClientFactory;
  const controller = new ConversationController({
    ...(options.controllerOptions ?? {}),
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
  return { key: options.key, controller };
}

export function releaseConversationScope(key: ConversationScopeKey): void {
  const entry = registry.get(key);
  if (entry === undefined) return;
  entry.refCount -= 1;
  if (entry.refCount > 0) return;
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

export function peekConversationScope(
  key: ConversationScopeKey,
): ConversationController | null {
  return registry.get(key)?.controller ?? null;
}

export function activeConversationScopeCount(): number {
  return registry.size;
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
