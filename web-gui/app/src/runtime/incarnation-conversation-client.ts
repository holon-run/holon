import {
  ConversationHttpError,
  CONVERSATION_INCARNATION_CAPABILITY,
  type ConversationClientLike,
} from "@holon/conversation-sdk";

import { RuntimeHttpError } from "./client";

export interface ConversationIdentityFence {
  readonly agentId: string;
  readonly incarnation: number;
  getAgentIdentity(agentId: string): Promise<{
    readonly identity: { readonly incarnation: number; readonly status: string };
  }>;
}

/**
 * Fence decoded results, not fetch responses: the agent may be replaced while
 * JSON decoding or stream iteration is in flight.
 */
export function pinConversationClient(
  client: ConversationClientLike,
  fence: ConversationIdentityFence,
): ConversationClientLike {
  let rejectedIdentity: ConversationHttpError | undefined;
  let negotiation: Promise<{ client: ConversationClientLike; capability: unknown; bound: boolean }> | undefined;
  let negotiationSignal: AbortSignal | undefined;
  function negotiate(signal?: AbortSignal) {
    if (negotiationSignal?.aborted) negotiation = undefined;
    if (negotiation) return negotiation;
    negotiationSignal = signal;
    const pending = client.requireCapability(signal).then((capability) => {
      signal?.throwIfAborted();
      const capabilities = (capability as { capabilities?: unknown } | null)?.capabilities;
      const bound = Array.isArray(capabilities) && capabilities.includes(CONVERSATION_INCARNATION_CAPABILITY);
      if (bound && client.withIncarnationFence === undefined) {
        throw new ConversationHttpError(409, {
          ok: false, code: "conversation_identity_binding_unsupported",
          error: "The negotiated incarnation binding requires a validating SDK client.",
          retryable: false,
        });
      }
      if (negotiation === pending) negotiationSignal = undefined;
      return {
        client: bound ? client.withIncarnationFence!({ agentId: fence.agentId, incarnation: fence.incarnation }) : client,
        capability, bound,
      };
    }).catch((error: unknown) => {
      if (negotiation === pending) negotiation = undefined;
      if (error instanceof ConversationHttpError && error.body.retryable === false) rejectedIdentity = error;
      throw error;
    });
    negotiation = pending;
    return pending;
  }
  async function negotiatedClient(signal?: AbortSignal) {
    signal?.throwIfAborted();
    const pending = negotiate(signal);
    if (signal === undefined) return pending;
    let onAbort!: () => void;
    const aborted = new Promise<never>((_resolve, reject) => {
      onAbort = () => reject(signal.reason);
      signal.addEventListener("abort", onAbort, { once: true });
    });
    try { return await Promise.race([pending, aborted]); }
    finally { signal.removeEventListener("abort", onAbort); }
  }
  async function verify(agentId: string, signal?: AbortSignal): Promise<void> {
    signal?.throwIfAborted();
    if (rejectedIdentity) throw rejectedIdentity;
    try {
      const { identity } = await fence.getAgentIdentity(fence.agentId);
      signal?.throwIfAborted();
      if (rejectedIdentity) throw rejectedIdentity;
      if (agentId !== fence.agentId || identity.incarnation !== fence.incarnation || identity.status !== "active") {
        rejectedIdentity = new ConversationHttpError(403, {
          ok: false,
          code: "conversation_identity_changed",
          error: "The conversation agent identity is no longer available.",
          retryable: false,
        });
        throw rejectedIdentity;
      }
    } catch (error) {
      if (error instanceof RuntimeHttpError && [401, 403, 404].includes(error.status)) {
        rejectedIdentity = new ConversationHttpError(error.status, {
          ok: false, code: "conversation_identity_unavailable",
          error: error.message, retryable: false,
        });
        throw rejectedIdentity;
      }
      throw error;
    }
  }
  async function checked<T>(operation: (source: ConversationClientLike, capability: unknown) => Promise<T>, agentId: string, signal?: AbortSignal): Promise<T> {
    signal?.throwIfAborted();
    if (rejectedIdentity) throw rejectedIdentity;
    if (agentId !== fence.agentId) {
      rejectedIdentity = new ConversationHttpError(403, {
        ok: false, code: "conversation_identity_changed",
        error: "The conversation agent identity is no longer available.", retryable: false,
      });
      throw rejectedIdentity;
    }
    const negotiated = await negotiatedClient(signal);
    signal?.throwIfAborted();
    try {
      const decoded = await operation(negotiated.client, negotiated.capability);
      signal?.throwIfAborted();
      if (rejectedIdentity) throw rejectedIdentity;
      if (!negotiated.bound) await verify(agentId, signal);
      return decoded;
    } catch (error) {
      if (error instanceof ConversationHttpError && error.body.retryable === false) rejectedIdentity = error;
      throw error;
    }
  }
  return {
    baseUrl: client.baseUrl,
    requireCapability: (signal) =>
      checked(async (_source, capability) => capability, fence.agentId, signal),
    summary: (agentId, options) =>
      checked((source) => source.summary(agentId, options), agentId, options?.signal),
    activities: (agentId, turnId, options) =>
      checked((source) => source.activities(agentId, turnId, options), agentId, options?.signal),
    brief: (agentId, briefId, signal) =>
      checked((source) => source.brief(agentId, briefId, signal), agentId, signal),
    ...(client.locateSource === undefined ? {} : {
      locateSource: (agentId, sourceRef, options) =>
        checked((source) => source.locateSource!(agentId, sourceRef, options), agentId, options?.signal),
    } satisfies Pick<ConversationClientLike, "locateSource">),
    async *stream(agentId, options) {
      options?.signal?.throwIfAborted();
      if (rejectedIdentity) throw rejectedIdentity;
      if (agentId !== fence.agentId) await checked(async () => undefined, agentId, options?.signal);
      const negotiated = await negotiatedClient(options?.signal);
      options?.signal?.throwIfAborted();
      try {
        for await (const item of negotiated.client.stream(agentId, options)) {
          options?.signal?.throwIfAborted();
          if (rejectedIdentity) throw rejectedIdentity;
          if (!negotiated.bound) await verify(agentId, options?.signal);
          yield item;
        }
      } catch (error) {
        if (error instanceof ConversationHttpError && error.body.retryable === false) rejectedIdentity = error;
        throw error;
      }
    },
  };
}
