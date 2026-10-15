import {
  ConversationHttpError,
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
        throw new ConversationHttpError(error.status, {
          ok: false, code: "conversation_identity_unavailable",
          error: error.message, retryable: false,
        });
      }
      throw error;
    }
  }
  async function checked<T>(result: Promise<T>, agentId: string, signal?: AbortSignal): Promise<T> {
    const decoded = await result;
    await verify(agentId, signal);
    return decoded;
  }
  return {
    baseUrl: client.baseUrl,
    requireCapability: (signal) =>
      checked(client.requireCapability(signal), fence.agentId, signal),
    summary: (agentId, options) =>
      checked(client.summary(agentId, options), agentId, options?.signal),
    activities: (agentId, turnId, options) =>
      checked(client.activities(agentId, turnId, options), agentId, options?.signal),
    brief: (agentId, briefId, signal) =>
      checked(client.brief(agentId, briefId, signal), agentId, signal),
    ...(client.locateSource === undefined ? {} : {
      locateSource: (agentId, sourceRef, options) =>
        checked(client.locateSource!(agentId, sourceRef, options), agentId, options?.signal),
    } satisfies Pick<ConversationClientLike, "locateSource">),
    async *stream(agentId, options) {
      for await (const item of client.stream(agentId, options)) {
        await verify(agentId, options?.signal);
        yield item;
      }
    },
  };
}
