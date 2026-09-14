import {
  ConversationCapabilityError,
  ConversationCompatibilityError,
  ConversationDecodeError,
  ConversationHttpError,
  ConversationProtocolError,
  ConversationResetError,
} from "./errors.js";
import {
  decodeBriefRecord,
  decodeConversationActivityResponse,
  decodeConversationHandshake,
  decodeConversationHttpError,
  decodeConversationSummaryResponse,
  isConversationResetReason,
} from "./decode.js";
import { ConversationBatchAssembler, parseSseStream } from "./sse.js";
import {
  CONVERSATION_CAPABILITY,
  HOLON_CONTROL_PROTOCOL_NAME,
  HOLON_CONTROL_PROTOCOL_VERSION,
  type BriefRecord,
  type ConversationActivityResponse,
  type ConversationCheckpoint,
  type ConversationDetailCursor,
  type ConversationHandshake,
  type ConversationHistoryCursor,
  type ConversationStreamItem,
  type ConversationSummaryResponse,
} from "./types.js";

export type FetchLike = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;

type MaybePromise<T> = T | Promise<T>;

export type ConversationHeadersProvider =
  | HeadersInit
  | (() => MaybePromise<HeadersInit>);

export type ConversationBearerTokenProvider =
  | string
  | (() => MaybePromise<string | undefined>);

export interface ConversationClientOptions {
  readonly baseUrl: string;
  readonly fetch?: FetchLike;
  readonly headers?: ConversationHeadersProvider;
  readonly bearerToken?: ConversationBearerTokenProvider;
}

export interface ConversationPageOptions {
  readonly limit?: number;
  readonly before?: ConversationHistoryCursor;
  readonly signal?: AbortSignal;
}

export interface ConversationActivityOptions {
  readonly limit?: number;
  readonly before?: ConversationDetailCursor;
  readonly signal?: AbortSignal;
}

export interface ConversationStreamOptions {
  readonly after?: ConversationCheckpoint;
  readonly cursorTransport?: "header" | "query";
  readonly limit?: number;
  readonly activityLimit?: number;
  readonly signal?: AbortSignal;
}

export class ConversationClient {
  readonly baseUrl: string;
  readonly #fetch: FetchLike;
  readonly #headers: ConversationHeadersProvider | undefined;
  readonly #bearerToken: ConversationBearerTokenProvider | undefined;

  constructor(options: ConversationClientOptions) {
    this.baseUrl = options.baseUrl.replace(/\/+$/, "");
    if (this.baseUrl.length === 0) {
      throw new ConversationProtocolError("baseUrl must not be empty");
    }
    const injectedFetch = options.fetch ?? globalThis.fetch;
    if (injectedFetch === undefined) {
      throw new ConversationProtocolError(
        "fetch is unavailable; inject a browser/Node compatible fetch implementation",
      );
    }
    this.#fetch = injectedFetch.bind(globalThis);
    this.#headers = options.headers;
    this.#bearerToken = options.bearerToken;
  }

  async handshake(signal?: AbortSignal): Promise<ConversationHandshake> {
    return this.#getJson(
      "/handshake",
      signal,
      decodeConversationHandshake,
    );
  }

  async requireCapability(signal?: AbortSignal): Promise<ConversationHandshake> {
    const handshake = await this.handshake(signal);
    if (
      handshake.protocol.name !== HOLON_CONTROL_PROTOCOL_NAME ||
      handshake.protocol.version !== HOLON_CONTROL_PROTOCOL_VERSION
    ) {
      throw new ConversationCompatibilityError(
        handshake.protocol.name,
        handshake.protocol.version,
      );
    }
    if (!handshake.capabilities.includes(CONVERSATION_CAPABILITY)) {
      throw new ConversationCapabilityError(CONVERSATION_CAPABILITY);
    }
    return handshake;
  }

  async summary(
    agentId: string,
    options: ConversationPageOptions = {},
  ): Promise<ConversationSummaryResponse> {
    const query = new URLSearchParams();
    if (options.limit !== undefined) {
      query.set("limit", String(options.limit));
    }
    if (options.before !== undefined) {
      query.set("before", options.before);
    }
    return this.#getJson(
      this.#agentPath(agentId, `/conversation${query.size === 0 ? "" : `?${query}`}`),
      options.signal,
      decodeConversationSummaryResponse,
    );
  }

  async activities(
    agentId: string,
    turnId: string,
    options: ConversationActivityOptions = {},
  ): Promise<ConversationActivityResponse> {
    const query = new URLSearchParams();
    if (options.limit !== undefined) {
      query.set("limit", String(options.limit));
    }
    if (options.before !== undefined) {
      query.set("before", options.before);
    }
    return this.#getJson(
      this.#agentPath(
        agentId,
        `/turns/${encodeURIComponent(turnId)}/activities${query.size === 0 ? "" : `?${query}`}`,
      ),
      options.signal,
      decodeConversationActivityResponse,
    );
  }

  async brief(
    agentId: string,
    briefId: string,
    signal?: AbortSignal,
  ): Promise<BriefRecord> {
    return this.#getJson(
      this.#agentPath(agentId, `/briefs/${encodeURIComponent(briefId)}`),
      signal,
      decodeBriefRecord,
    );
  }

  async *stream(
    agentId: string,
    options: ConversationStreamOptions = {},
  ): AsyncGenerator<ConversationStreamItem> {
    const query = new URLSearchParams();
    const cursorTransport = options.cursorTransport ?? "header";
    if (options.after !== undefined && cursorTransport === "query") {
      query.set("after", options.after);
    }
    if (options.limit !== undefined) {
      query.set("limit", String(options.limit));
    }
    if (options.activityLimit !== undefined) {
      query.set("activity_limit", String(options.activityLimit));
    }
    const headers = await this.#requestHeaders();
    headers.set("accept", "text/event-stream");
    if (options.after !== undefined && cursorTransport === "header") {
      headers.set("last-event-id", options.after);
    }
    const response = await this.#fetch(
      this.#url(
        this.#agentPath(
          agentId,
          `/conversation/stream${query.size === 0 ? "" : `?${query}`}`,
        ),
      ),
      {
        method: "GET",
        headers,
        ...(options.signal === undefined ? {} : { signal: options.signal }),
      },
    );
    if (!response.ok) {
      await this.#throwResponseError(response);
    }
    if (response.body === null) {
      throw new ConversationDecodeError(
        "$stream",
        "response has no readable body",
      );
    }
    const assembler = new ConversationBatchAssembler();
    try {
      for await (const frame of parseSseStream(response.body)) {
        const item = assembler.push(frame);
        if (item !== null) {
          yield item;
          if (item.type === "reset_required") {
            return;
          }
        }
      }
    } finally {
      assembler.discardIncompleteBatch();
    }
  }

  #agentPath(agentId: string, suffix: string): string {
    if (agentId.length === 0) {
      throw new ConversationProtocolError("agentId must not be empty");
    }
    return `/agents/${encodeURIComponent(agentId)}${suffix}`;
  }

  #url(path: string): string {
    return `${this.baseUrl}${path}`;
  }

  async #requestHeaders(): Promise<Headers> {
    const provided =
      typeof this.#headers === "function"
        ? await this.#headers()
        : this.#headers;
    const headers = new Headers(provided);
    if (this.#bearerToken !== undefined) {
      const token =
        typeof this.#bearerToken === "function"
          ? await this.#bearerToken()
          : this.#bearerToken;
      if (token !== undefined) {
        headers.set("authorization", `Bearer ${token}`);
      }
    }
    return headers;
  }

  async #getJson<T>(
    path: string,
    signal: AbortSignal | undefined,
    decode: (value: unknown) => T,
  ): Promise<T> {
    const headers = await this.#requestHeaders();
    headers.set("accept", "application/json");
    const response = await this.#fetch(this.#url(path), {
      method: "GET",
      headers,
      ...(signal === undefined ? {} : { signal }),
    });
    if (!response.ok) {
      await this.#throwResponseError(response);
    }
    let value: unknown;
    try {
      value = await response.json();
    } catch (error) {
      throw new ConversationDecodeError("$response", "invalid JSON", {
        cause: error,
      });
    }
    return decode(value);
  }

  async #throwResponseError(response: Response): Promise<never> {
    let value: unknown;
    try {
      value = await response.json();
    } catch (error) {
      throw new ConversationProtocolError(
        `HTTP ${response.status} returned a non-JSON error`,
        { cause: error },
      );
    }
    const body = decodeConversationHttpError(value, "$response");
    if (body.code === "capability_unavailable") {
      throw new ConversationCapabilityError(CONVERSATION_CAPABILITY);
    }
    if (
      body.code === "conversation_reset_required" &&
      isConversationResetReason(body.reason)
    ) {
      throw new ConversationResetError(body.reason, {
        oldestRetainedSeq: resetSequence(
          body.oldest_retained_seq,
          "$response.oldest_retained_seq",
        ),
        eventHeadSeq: resetSequence(
          body.event_head_seq,
          "$response.event_head_seq",
        ),
        ...(body.hint === undefined ? {} : { hint: body.hint }),
      });
    }
    throw new ConversationHttpError(response.status, body);
  }
}

function resetSequence(value: unknown, path: string): number | null {
  if (value === undefined || value === null) {
    return null;
  }
  if (
    typeof value !== "number" ||
    !Number.isSafeInteger(value) ||
    value < 0
  ) {
    throw new ConversationDecodeError(
      path,
      "expected non-negative safe integer or null",
    );
  }
  return value;
}
