import { describe, expect, it, vi } from "vitest";

import { READ_STATE_BUS_CHANNEL, ReadStateBus } from "./read-state-bus";

describe("ReadStateBus", () => {
  it("delivers server revalidation notifications between tabs", async () => {
    const received: string[] = [];
    const receiver = new ReadStateBus((message) => received.push(message.agentId));
    const sender = new ReadStateBus(() => undefined);

    sender.publish({
      kind: "server_revalidation_required",
      remoteKey: "http://127.0.0.1:7878",
      agentId: "agent-a",
    });
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(received).toEqual(["agent-a"]);
    receiver.dispose();
    sender.dispose();
  });

  it("ignores malformed messages posted by foreign code", async () => {
    const handler = vi.fn();
    const receiver = new ReadStateBus(handler);
    const foreign = new BroadcastChannel(READ_STATE_BUS_CHANNEL);
    foreign.postMessage({ kind: "read_state_changed", remoteKey: "r", agentId: "a" });
    foreign.postMessage("not-an-object");
    foreign.postMessage({ kind: "something_else", remoteKey: "r", agentId: "a" });
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(handler).not.toHaveBeenCalled();
    foreign.close();
    receiver.dispose();
  });

  it("stops delivering after dispose", async () => {
    const handler = vi.fn();
    const receiver = new ReadStateBus(handler);
    const sender = new ReadStateBus(() => undefined);
    receiver.dispose();
    sender.publish({ kind: "server_revalidation_required", remoteKey: "r", agentId: "agent-a" });
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(handler).not.toHaveBeenCalled();
    sender.dispose();
  });
});
