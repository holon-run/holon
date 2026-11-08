import { AGENT_AVATAR_VERSION, type AgentAvatarExpression } from "../lib/agent-avatar";

export const AGENT_AVATAR_CAPABILITY = "agent_avatar_v1";

export interface AvatarResourceRequest {
  agentId: string;
  version: string;
  expression: AgentAvatarExpression;
  size: 32 | 64 | 128 | 256 | 512;
}

type AvatarLoader = (agentId: string, options: Omit<AvatarResourceRequest, "agentId"> & {
  signal: AbortSignal;
}) => Promise<{ blob: Blob }>;

interface Entry {
  key: string;
  request: AvatarResourceRequest;
  controller: AbortController;
  listeners: Set<() => void>;
  refs: number;
  started: boolean;
  settled: boolean;
  blob?: Blob;
  url?: string;
}

interface Scope {
  epoch: number;
  loader?: AvatarLoader;
  running: number;
}

/** Private, memory-only image resources. A cached Blob is never an authorization grant. */
export class AgentAvatarResources {
  private scope: Scope = { epoch: 0, running: 0 };
  private readonly entries = new Map<string, Entry>();
  private readonly listeners = new Set<() => void>();
  private bytes = 0;

  constructor(private readonly limits = {
    entries: 128, bytes: 8 * 1024 * 1024, imageBytes: 2 * 1024 * 1024, concurrency: 4,
  }) {}

  getEpoch = (): number => this.scope.epoch;
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  configure(loader: AvatarLoader, capabilities: readonly string[]): void {
    const supportedLoader = capabilities.includes(AGENT_AVATAR_CAPABILITY) ? loader : undefined;
    if (supportedLoader !== this.scope.loader) this.reset(supportedLoader);
  }

  reset = (loader?: AvatarLoader): void => {
    const oldEntries = [...this.entries.values()];
    this.scope = { epoch: this.scope.epoch + 1, loader, running: 0 };
    this.entries.clear();
    this.bytes = 0;
    for (const entry of oldEntries) {
      entry.controller.abort();
      if (entry.url) URL.revokeObjectURL(entry.url);
      entry.blob = undefined;
      entry.url = undefined;
      entry.listeners.forEach((listener) => listener());
    }
    this.listeners.forEach((listener) => listener());
  };

  peek(request: AvatarResourceRequest, epoch: number): string | undefined {
    return epoch === this.scope.epoch ? this.entries.get(resourceKey(request))?.url : undefined;
  }

  retain(request: AvatarResourceRequest, listener: () => void, epoch: number): () => void {
    if (epoch !== this.scope.epoch || !this.scope.loader) return () => {};
    const key = resourceKey(request);
    let entry = this.entries.get(key);
    if (!entry) {
      if (!this.makeRoom(1, 0)) return () => {};
      entry = {
        key, request, controller: new AbortController(), listeners: new Set(),
        refs: 0, started: false, settled: false,
      };
      this.entries.set(key, entry);
    } else if (entry.settled && !entry.blob) {
      // A failed request is retryable on a later subscription, not a capability verdict.
      entry.controller = new AbortController();
      entry.started = false;
      entry.settled = false;
    }
    const retained = entry;
    retained.refs += 1;
    retained.listeners.add(listener);
    this.touch(retained);
    if (retained.blob && !retained.url) retained.url = URL.createObjectURL(retained.blob);
    this.pump();
    let released = false;
    return () => {
      if (released) return;
      released = true;
      retained.listeners.delete(listener);
      retained.refs -= 1;
      if (retained.refs !== 0) return;
      if (retained.url) URL.revokeObjectURL(retained.url);
      retained.url = undefined;
      if (this.entries.get(key) !== retained) return;
      if (!retained.blob) {
        this.entries.delete(key);
        retained.controller.abort();
      } else {
        this.touch(retained);
      }
    };
  }

  private touch(entry: Entry): void {
    this.entries.delete(entry.key);
    this.entries.set(entry.key, entry);
  }

  private makeRoom(extraEntries: number, extraBytes: number): boolean {
    for (const [key, entry] of this.entries) {
      if (this.entries.size + extraEntries <= this.limits.entries
        && this.bytes + extraBytes <= this.limits.bytes) return true;
      if (entry.refs !== 0) continue;
      this.bytes -= entry.blob?.size ?? 0;
      this.entries.delete(key);
    }
    return this.entries.size + extraEntries <= this.limits.entries
      && this.bytes + extraBytes <= this.limits.bytes;
  }

  private pump(): void {
    const scope = this.scope;
    const loader = scope.loader;
    if (!loader) return;
    for (const entry of this.entries.values()) {
      if (scope.running >= this.limits.concurrency) break;
      if (entry.started || entry.settled || entry.refs === 0) continue;
      entry.started = true;
      scope.running += 1;
      void Promise.resolve().then(() => {
        entry.controller.signal.throwIfAborted();
        return loader(entry.request.agentId, { ...entry.request, signal: entry.controller.signal });
      }).then(({ blob }) => {
        if (scope !== this.scope || entry.controller.signal.aborted
          || this.entries.get(entry.key) !== entry || entry.refs === 0) return;
        if (blob.type !== "image/png" || blob.size === 0 || blob.size > this.limits.imageBytes
          || !this.makeRoom(0, blob.size)) return;
        entry.url = URL.createObjectURL(blob);
        entry.blob = blob;
        this.bytes += blob.size;
      }).catch(() => {
        // Network/auth/target/overload errors keep the local renderer available.
      }).finally(() => {
        scope.running -= 1;
        if (scope !== this.scope || this.entries.get(entry.key) !== entry) {
          if (scope === this.scope) this.pump();
          return;
        }
        entry.settled = true;
        entry.listeners.forEach((listener) => listener());
        this.pump();
      });
    }
  }
}

export function avatarResourceRequest(
  agentId: string, expression: AgentAvatarExpression,
): AvatarResourceRequest {
  return { agentId, version: AGENT_AVATAR_VERSION, expression, size: 128 };
}

function resourceKey(request: AvatarResourceRequest): string {
  return JSON.stringify([request.agentId, request.version, request.size, request.expression]);
}

export const agentAvatarResources = new AgentAvatarResources();
