import type { ConversationClientLike } from "@holon/conversation-sdk";

/**
 * SDK pause retains history but does not stop detail/history requests. Fence
 * their decoded delivery too. An interrupted demand parks until verification
 * resumes the scope, then refetches rather than accepting its old response.
 * This lifecycle gate does not replace the incarnation fence.
 */
export function conversationNetworkGate(client: ConversationClientLike) {
  let active = new AbortController();
  let disposed = false;
  active.abort();
  const resumed = new Set<() => void>();
  const signalFor = (signal?: AbortSignal) => {
    active.signal.throwIfAborted();
    return signal ? AbortSignal.any([active.signal, signal]) : active.signal;
  };
  function waitForResume(signal?: AbortSignal): Promise<void> {
    signal?.throwIfAborted();
    if (disposed) return Promise.reject(new DOMException("Conversation disposed", "AbortError"));
    if (!active.signal.aborted) return Promise.resolve();
    return new Promise((resolve, reject) => {
      const cleanup = () => { resumed.delete(wake); signal?.removeEventListener("abort", cancel); };
      const cancel = () => { cleanup(); reject(signal?.reason); };
      const wake = () => {
        cleanup();
        if (disposed) reject(new DOMException("Conversation disposed", "AbortError"));
        else resolve();
      };
      resumed.add(wake);
      signal?.addEventListener("abort", cancel, { once: true });
    });
  }
  function checked<T>(caller: AbortSignal | undefined, request: (signal: AbortSignal) => Promise<T>): Promise<T> {
    active.signal.throwIfAborted();
    return run();
    async function run(): Promise<T> {
      for (;;) {
        await waitForResume(caller);
        const generation = active;
        const signal = signalFor(caller);
        let cancel: (() => void) | undefined;
        try {
          signal.throwIfAborted();
          const cancelled = new Promise<never>((_, reject) => {
            cancel = () => reject(signal.reason);
            signal.addEventListener("abort", cancel, { once: true });
          });
          const decoded = await Promise.race([request(signal), cancelled]);
          signal.throwIfAborted();
          return decoded;
        } catch (error) {
          if (!generation.signal.aborted || caller?.aborted || disposed) throw error;
        } finally {
          if (cancel) signal.removeEventListener("abort", cancel);
        }
      }
    }
  }
  const gated: ConversationClientLike = {
    baseUrl: client.baseUrl,
    requireCapability: (signal) => checked(signal, (bound) => client.requireCapability(bound)),
    summary: (agentId, options) => checked(options?.signal, (signal) => client.summary(agentId, { ...options, signal })),
    activities: (agentId, turnId, options) => checked(options?.signal, (signal) => client.activities(agentId, turnId, { ...options, signal })),
    brief: (agentId, briefId, signal) => checked(signal, (bound) => client.brief(agentId, briefId, bound)),
    ...(client.locateSource === undefined ? {} : {
      locateSource: (agentId, sourceRef, options) => checked(options?.signal,
        (signal) => client.locateSource!(agentId, sourceRef, { ...options, signal })),
    } satisfies Pick<ConversationClientLike, "locateSource">),
    async *stream(agentId, options) {
      const signal = signalFor(options?.signal);
      for await (const item of client.stream(agentId, { ...options, signal })) {
        signal.throwIfAborted();
        yield item;
      }
    },
  };
  return {
    client: gated,
    resume() {
      if (disposed) return;
      if (active.signal.aborted) active = new AbortController();
      for (const wake of [...resumed]) wake();
    },
    pause() { active.abort(); },
    dispose() {
      disposed = true;
      active.abort();
      for (const wake of [...resumed]) wake();
    },
  };
}
