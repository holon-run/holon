// @vitest-environment happy-dom
import { describe, expect, it, vi } from "vitest";

import { createRosterHintStream, type RosterHintStreamDependencies } from "./roster-hint-stream";

function hint(agentId: string): string {
  return `event: agent_roster_hint\ndata: {"agent_id":"${agentId}"}\n\n`;
}

/** Frames enqueue immediately; the stream deliberately stays open. */
function liveStream(chunks: string[]): Response {
  const encoder = new TextEncoder();
  return new Response(
    new ReadableStream<Uint8Array>({
      start(controller) {
        for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
      },
    }),
    { status: 200, headers: { "content-type": "text/event-stream" } },
  );
}

/** A completed body models the server closing the stream (for example lag). */
function closedStream(chunks: string[], status = 200): Response {
  return new Response(chunks.join(""), {
    status,
    headers: { "content-type": "text/event-stream" },
  });
}

const wait = (ms = 12) => new Promise((resolve) => setTimeout(resolve, ms));

function setup(overrides: Partial<RosterHintStreamDependencies> = {}) {
  const openStream = vi.fn((_signal: AbortSignal) => Promise.resolve(liveStream([])));
  const onHint = vi.fn();
  const isHidden = vi.fn(() => false);
  const controller = createRosterHintStream({
    openStream: (signal) => openStream(signal),
    onHint,
    isHidden,
    retryDelayMs: () => 1,
    ...overrides,
  });
  return { controller, openStream, onHint, isHidden };
}

describe("createRosterHintStream", () => {
  it("emits one visible hint per frame and ignores heartbeats and other events", async () => {
    const { controller, openStream, onHint } = setup();
    openStream.mockImplementation(async () =>
      liveStream([
        ": heartbeat\n\n",
        hint("agent-a"),
        hint("agent-b"),
        'event: something_else\ndata: {}\n\n',
      ]),
    );
    controller.start();
    await wait();
    expect(onHint).toHaveBeenCalledTimes(2);
    controller.stop();
  });

  it("keeps the existing subscription across repeated start calls", async () => {
    const { controller, openStream } = setup();
    controller.start();
    controller.start();
    await wait();
    expect(openStream).toHaveBeenCalledTimes(1);
    controller.stop();
  });

  it("aborts the open request on stop", async () => {
    const signals: AbortSignal[] = [];
    const { controller, openStream } = setup();
    openStream.mockImplementation(async (signal: AbortSignal) => {
      signals.push(signal);
      return liveStream([]);
    });
    controller.start();
    await wait();
    controller.stop();
    expect(signals[0].aborted).toBe(true);
  });

  it("suppresses snapshot pulls while hidden and flushes once when visible again", async () => {
    const { controller, openStream, onHint, isHidden } = setup();
    isHidden.mockReturnValue(true);
    openStream.mockImplementation(async () => liveStream([hint("agent-a"), hint("agent-b")]));
    controller.start();
    await wait();
    expect(onHint).not.toHaveBeenCalled();
    isHidden.mockReturnValue(false);
    document.dispatchEvent(new Event("visibilitychange"));
    expect(onHint).toHaveBeenCalledTimes(1);
    controller.stop();
  });

  it("reconnects with backoff after the server closes the stream", async () => {
    const { controller, openStream, onHint } = setup();
    openStream.mockImplementationOnce(async () => closedStream([hint("agent-a")]));
    openStream.mockImplementationOnce(async () => liveStream([hint("agent-b")]));
    controller.start();
    await wait(25);
    expect(openStream).toHaveBeenCalledTimes(2);
    // One hint from each connection, plus one recovery pull for the gap.
    expect(onHint).toHaveBeenCalledTimes(3);
    controller.stop();
  });

  it("retries transport failures with backoff and recovers", async () => {
    const { controller, openStream, onHint } = setup();
    openStream.mockRejectedValueOnce(new TypeError("fetch failed"));
    openStream.mockImplementationOnce(async () => liveStream([hint("agent-a")]));
    controller.start();
    await wait(25);
    expect(openStream).toHaveBeenCalledTimes(2);
    expect(onHint).toHaveBeenCalledTimes(2);
    controller.stop();
  });

  it("stops permanently on auth rejection without retrying", async () => {
    const { controller, openStream } = setup();
    openStream.mockImplementation(async () => closedStream([], 401));
    controller.start();
    await wait(25);
    expect(openStream).toHaveBeenCalledTimes(1);
    controller.stop();
  });

  it("stops permanently when the runtime does not expose the stream", async () => {
    const { controller, openStream } = setup();
    openStream.mockImplementation(async () => closedStream([], 404));
    controller.start();
    await wait(25);
    expect(openStream).toHaveBeenCalledTimes(1);
    controller.stop();
  });
});
