import assert from "node:assert/strict";
import test from "node:test";

import {
  ConversationBatchAssembler,
  ConversationDecodeError,
  parseSseStream,
} from "../dist/index.js";
import { sseFrame, turn } from "./helpers.mjs";

test("parses chunked SSE comments and multiline data", async () => {
  const encoder = new TextEncoder();
  const body = new ReadableStream({
    start(controller) {
      controller.enqueue(encoder.encode(": heartbeat\r\n\r\nevent: sample\r\n"));
      controller.enqueue(encoder.encode("id: opaque\r\ndata: {\"a\":\r\n"));
      controller.enqueue(encoder.encode("data: 1}\r\n\r\n"));
      controller.close();
    },
  });
  const frames = [];
  for await (const frame of parseSseStream(body)) {
    frames.push(frame);
  }
  assert.deepEqual(frames, [
    {
      event: "sample",
      id: "opaque",
      data: '{"a":\n1}',
    },
  ]);
});

test("discards an unterminated SSE event at EOF", async () => {
  const encoder = new TextEncoder();
  const body = new ReadableStream({
    start(controller) {
      controller.enqueue(
        encoder.encode(
          'event: checkpoint\nid: opaque\ndata: {"type":"checkpoint"}\n',
        ),
      );
      controller.close();
    },
  });
  const frames = [];
  for await (const frame of parseSseStream(body)) {
    frames.push(frame);
  }
  assert.deepEqual(frames, []);
});

test("assembles a complete batch and exposes only its checkpoint", () => {
  const assembler = new ConversationBatchAssembler();
  const begin = {
    type: "batch_begin",
    batch_id: "conversation:10:11",
    schema_version: 2,
    query_version: 2,
    runtime_id: "runtime-a",
    event_log_epoch: "epoch-a",
    visibility_scope_id: "scope-a",
    from_seq: 10,
    through_seq: 11,
  };
  const mutation = {
    type: "turn_summary_upsert",
    event_log_epoch: "epoch-a",
    visibility_scope_id: "scope-a",
    turn: turn("turn-10", 10, 2),
  };
  const checkpoint = {
    type: "checkpoint",
    batch_id: "conversation:10:11",
    event_log_epoch: "epoch-a",
    visibility_scope_id: "scope-a",
    through_seq: 11,
    checkpoint: "checkpoint-11",
  };
  assert.equal(assembler.push(sseFrame("batch_begin", begin)), null);
  assert.equal(
    assembler.push(sseFrame("turn_summary_upsert", mutation)),
    null,
  );
  const item = assembler.push(
    sseFrame("checkpoint", checkpoint, "checkpoint-11"),
  );
  assert.equal(item.type, "batch");
  assert.equal(item.batch.mutations.length, 1);
  assert.equal(item.batch.checkpoint.checkpoint, "checkpoint-11");
  assert.equal(assembler.hasIncompleteBatch, false);
});

test("never exposes partial batches and rejects mismatched checkpoint ids", () => {
  const assembler = new ConversationBatchAssembler();
  const begin = {
    type: "batch_begin",
    batch_id: "conversation:10:11",
    schema_version: 2,
    query_version: 2,
    runtime_id: "runtime-a",
    event_log_epoch: "epoch-a",
    visibility_scope_id: "scope-a",
    from_seq: 10,
    through_seq: 11,
  };
  assembler.push(sseFrame("batch_begin", begin));
  assert.equal(assembler.hasIncompleteBatch, true);
  assembler.discardIncompleteBatch();
  assert.equal(assembler.hasIncompleteBatch, false);

  assembler.push(sseFrame("batch_begin", begin));
  assert.throws(
    () =>
      assembler.push(
        sseFrame(
          "checkpoint",
          {
            type: "checkpoint",
            batch_id: begin.batch_id,
            event_log_epoch: "epoch-a",
            visibility_scope_id: "scope-a",
            through_seq: 11,
            checkpoint: "checkpoint-11",
          },
          "different-checkpoint",
        ),
      ),
    ConversationDecodeError,
  );
});
