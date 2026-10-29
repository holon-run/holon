import { cacheClearRemoteSessions, cacheDeleteSession } from "./idb-cache";
import { currentRemoteKey } from "./session-cache";
import type { RuntimeConnectionConfig } from "./types";

export interface SessionCacheContext {
  remoteKey: string;
  generation: number;
}

export function isSessionCacheContextCurrent(
  context: SessionCacheContext,
  remoteKey: string,
  generation: number,
): boolean {
  return context.remoteKey === remoteKey && context.generation === generation;
}

export interface AgentSessionRepositoryDependencies {
  getConnectionConfig: () => RuntimeConnectionConfig;
  getGeneration: () => number;
}

/** Cleanup for the retired per-agent session cache, not a content source. */
export class AgentSessionRepository {
  private cacheInitPromise: Promise<void> | null = null;

  constructor(private readonly dependencies: AgentSessionRepositoryDependencies) {}

  initializeCache(): void {
    if (this.cacheInitPromise) return;
    const context = this.currentCacheContext();
    const initialization = this.initializeCacheForContext(context);
    this.cacheInitPromise = initialization;
    void initialization.finally(() => {
      if (this.cacheInitPromise === initialization) this.cacheInitPromise = null;
    });
  }

  private async initializeCacheForContext(context: SessionCacheContext): Promise<void> {
    try {
      // Retire only legacy session records; conversation and model caches have
      // independent lifecycles and must not be reset by old session schemas.
      await cacheClearRemoteSessions(context.remoteKey, () => {
        const current = this.currentCacheContext();
        return isSessionCacheContextCurrent(context, current.remoteKey, current.generation);
      });
    } catch (error) {
      console.warn("Failed to clear the legacy agent session cache.", error);
    }
  }

  async deleteCachedSession(agentId: string): Promise<void> {
    await cacheDeleteSession(
      currentRemoteKey(this.dependencies.getConnectionConfig()),
      agentId,
    );
  }

  switchRemote(): void {
    this.cacheInitPromise = null;
  }

  private currentCacheContext(): SessionCacheContext {
    return {
      remoteKey: currentRemoteKey(this.dependencies.getConnectionConfig()),
      generation: this.dependencies.getGeneration(),
    };
  }
}
