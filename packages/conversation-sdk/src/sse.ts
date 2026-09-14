import { ConversationDecodeError } from "./errors.js";
import { decodeConversationStreamMessage } from "./decode.js";
import type {
  BatchBeginMessage,
  CheckpointMessage,
  ConversationMutationMessage,
  ConversationStreamItem,
} from "./types.js";

export interface SseFrame {
  readonly event: string;
  readonly data: string;
  readonly id?: string;
}

export async function* parseSseStream(
  body: ReadableStream<Uint8Array>,
): AsyncGenerator<SseFrame> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let event = "";
  let id: string | undefined;
  let data: string[] = [];
  let completed = false;

  const dispatch = (): SseFrame | null => {
    if (data.length === 0) {
      event = "";
      id = undefined;
      return null;
    }
    const frame: SseFrame = {
      event: event || "message",
      data: data.join("\n"),
      ...(id === undefined ? {} : { id }),
    };
    event = "";
    id = undefined;
    data = [];
    return frame;
  };

  const consumeLine = (line: string): SseFrame | null => {
    if (line === "") {
      return dispatch();
    }
    if (line.startsWith(":")) {
      return null;
    }
    const separator = line.indexOf(":");
    const field = separator === -1 ? line : line.slice(0, separator);
    let value = separator === -1 ? "" : line.slice(separator + 1);
    if (value.startsWith(" ")) {
      value = value.slice(1);
    }
    switch (field) {
      case "event":
        event = value;
        break;
      case "data":
        data.push(value);
        break;
      case "id":
        if (!value.includes("\0")) {
          id = value;
        }
        break;
      default:
        break;
    }
    return null;
  };

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        decoder.decode();
        completed = true;
        break;
      }
      buffer += decoder.decode(value, { stream: true });
      while (true) {
        const newline = buffer.indexOf("\n");
        if (newline === -1) {
          break;
        }
        let line = buffer.slice(0, newline);
        buffer = buffer.slice(newline + 1);
        if (line.endsWith("\r")) {
          line = line.slice(0, -1);
        }
        const frame = consumeLine(line);
        if (frame !== null) {
          yield frame;
        }
      }
    }
  } finally {
    if (!completed) {
      await reader.cancel().catch(() => undefined);
    }
    reader.releaseLock();
  }
}

interface PendingBatch {
  readonly begin: BatchBeginMessage;
  readonly mutations: ConversationMutationMessage[];
}

export class ConversationBatchAssembler {
  #pending: PendingBatch | null = null;

  push(frame: SseFrame): ConversationStreamItem | null {
    let json: unknown;
    try {
      json = JSON.parse(frame.data);
    } catch (error) {
      throw new ConversationDecodeError("$stream.data", "invalid JSON", {
        cause: error,
      });
    }
    const message = decodeConversationStreamMessage(json, "$stream.data");
    if (frame.event !== message.type) {
      throw new ConversationDecodeError(
        "$stream.event",
        `event ${JSON.stringify(frame.event)} does not match message type ${JSON.stringify(message.type)}`,
      );
    }

    if (message.type === "reset_required") {
      if (frame.id !== undefined) {
        throw new ConversationDecodeError(
          "$stream.id",
          "reset_required must not carry an SSE id",
        );
      }
      if (this.#pending !== null) {
        throw new ConversationDecodeError(
          "$stream",
          "reset_required interrupted an incomplete batch",
        );
      }
      return { type: "reset_required", reset: message };
    }

    if (message.type === "batch_begin") {
      if (frame.id !== undefined) {
        throw new ConversationDecodeError(
          "$stream.id",
          "batch_begin must not carry an SSE id",
        );
      }
      if (this.#pending !== null) {
        throw new ConversationDecodeError(
          "$stream",
          "received batch_begin before the prior checkpoint",
        );
      }
      this.#pending = { begin: message, mutations: [] };
      return null;
    }

    const pending = this.#pending;
    if (pending === null) {
      throw new ConversationDecodeError(
        "$stream",
        `${message.type} appeared outside a batch`,
      );
    }

    if (message.type === "checkpoint") {
      this.#validateCheckpoint(pending.begin, message, frame.id);
      this.#pending = null;
      return {
        type: "batch",
        batch: {
          begin: pending.begin,
          mutations: pending.mutations,
          checkpoint: message,
        },
      };
    }

    if (frame.id !== undefined) {
      throw new ConversationDecodeError(
        "$stream.id",
        `${message.type} must not carry an SSE id`,
      );
    }
    this.#validateScope(pending.begin, message);
    pending.mutations.push(message);
    return null;
  }

  discardIncompleteBatch(): void {
    this.#pending = null;
  }

  get hasIncompleteBatch(): boolean {
    return this.#pending !== null;
  }

  #validateScope(
    begin: BatchBeginMessage,
    message: ConversationMutationMessage,
  ): void {
    if (message.event_log_epoch !== begin.event_log_epoch) {
      throw new ConversationDecodeError(
        "$stream.data.event_log_epoch",
        "mutation epoch does not match batch_begin",
      );
    }
    if (message.visibility_scope_id !== begin.visibility_scope_id) {
      throw new ConversationDecodeError(
        "$stream.data.visibility_scope_id",
        "mutation visibility scope does not match batch_begin",
      );
    }
  }

  #validateCheckpoint(
    begin: BatchBeginMessage,
    checkpoint: CheckpointMessage,
    eventId: string | undefined,
  ): void {
    if (checkpoint.batch_id !== begin.batch_id) {
      throw new ConversationDecodeError(
        "$stream.data.batch_id",
        "checkpoint batch id does not match batch_begin",
      );
    }
    if (checkpoint.through_seq !== begin.through_seq) {
      throw new ConversationDecodeError(
        "$stream.data.through_seq",
        "checkpoint boundary does not match batch_begin",
      );
    }
    if (checkpoint.event_log_epoch !== begin.event_log_epoch) {
      throw new ConversationDecodeError(
        "$stream.data.event_log_epoch",
        "checkpoint epoch does not match batch_begin",
      );
    }
    if (checkpoint.visibility_scope_id !== begin.visibility_scope_id) {
      throw new ConversationDecodeError(
        "$stream.data.visibility_scope_id",
        "checkpoint visibility scope does not match batch_begin",
      );
    }
    if (eventId === undefined) {
      throw new ConversationDecodeError(
        "$stream.id",
        "checkpoint must carry the resumable SSE id",
      );
    }
    if (eventId !== checkpoint.checkpoint) {
      throw new ConversationDecodeError(
        "$stream.id",
        "checkpoint SSE id does not match its opaque checkpoint",
      );
    }
  }
}
