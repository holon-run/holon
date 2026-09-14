import type {
  ConversationResetReason,
  ResetRequiredMessage,
} from "./types.js";

export class ConversationProtocolError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = new.target.name;
  }
}

export class ConversationDecodeError extends ConversationProtocolError {
  readonly path: string;

  constructor(path: string, message: string, options?: ErrorOptions) {
    super(`${path}: ${message}`, options);
    this.path = path;
  }
}

export interface ConversationHttpErrorBody {
  readonly ok: false;
  readonly error: string;
  readonly code?: string;
  readonly hint?: string;
  readonly retryable?: boolean;
  readonly [key: string]: unknown;
}

export class ConversationHttpError extends ConversationProtocolError {
  readonly status: number;
  readonly body: ConversationHttpErrorBody;

  constructor(status: number, body: ConversationHttpErrorBody) {
    super(body.error);
    this.status = status;
    this.body = body;
  }
}

export class ConversationCapabilityError extends ConversationProtocolError {
  readonly capability: string;

  constructor(capability: string) {
    super(`required capability is unavailable: ${capability}`);
    this.capability = capability;
  }
}

export class ConversationCompatibilityError extends ConversationProtocolError {
  readonly protocolName: string;
  readonly protocolVersion: number;

  constructor(protocolName: string, protocolVersion: number) {
    super(
      `unsupported control protocol: expected holon-control@1, received ${protocolName}@${protocolVersion}`,
    );
    this.protocolName = protocolName;
    this.protocolVersion = protocolVersion;
  }
}

export class ConversationResetError extends ConversationProtocolError {
  readonly reason: ConversationResetReason;
  readonly oldestRetainedSeq: number | null;
  readonly eventHeadSeq: number | null;
  readonly hint?: string;

  constructor(
    reason: ConversationResetReason,
    options: {
      readonly oldestRetainedSeq?: number | null;
      readonly eventHeadSeq?: number | null;
      readonly hint?: string;
      readonly cause?: unknown;
    } = {},
  ) {
    super(`conversation reset required: ${reason}`, {
      cause: options.cause,
    });
    this.reason = reason;
    this.oldestRetainedSeq = options.oldestRetainedSeq ?? null;
    this.eventHeadSeq = options.eventHeadSeq ?? null;
    if (options.hint !== undefined) {
      this.hint = options.hint;
    }
  }

  static fromStream(reset: ResetRequiredMessage): ConversationResetError {
    return new ConversationResetError(reset.reason, {
      oldestRetainedSeq: reset.oldest_retained_seq,
      eventHeadSeq: reset.event_head_seq,
      hint: reset.hint,
    });
  }
}

export class ConversationStaleResponseError extends ConversationProtocolError {}

export class ConversationStateLimitError extends ConversationProtocolError {
  readonly resource: string;
  readonly limit: number;

  constructor(resource: string, limit: number) {
    super(`conversation client state exceeded ${resource} limit ${limit}`);
    this.resource = resource;
    this.limit = limit;
  }
}
